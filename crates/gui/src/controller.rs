//! Slint callback registration and UI model updates.

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use copypasta::{ClipboardContext, ClipboardProvider as _};
use ephervex_core::live_diagnostics::{LogFilter, LogLevel, RetainedLog};
use ephervex_core::{ModHost, OptionalSelection, ProviderFileIdentity, WindowSize};
use ephervex_i18n::{Arg, lookup, lookup_args};
use ephervex_model::AbsentModPolicy;
use ephervex_model::ModProvider;
use slint::{
    ComponentHandle as _, Image, LogicalSize, Model as _, ModelRc, SharedString, Timer, TimerMode,
    VecModel,
};

use crate::backend::{
    self, AccountData, ActivityData, ArtifactCacheStatusData, Command, DiagnosticContent,
    DiagnosticData, Event, JavaData, LiveLogLine, LiveLogSource, LiveLogUpdate, ManagedModData,
    ModSearchData, ServerPackTarget, ServerWorkspaceData, SettingsData, Snapshot,
};
use crate::file_drop::{FileDrop, FileDrops};
use crate::minecraft_defaults::{
    self, LanguageCatalogState, MinecraftDefaultsModel, WindowAspect, WindowAxis, window_anchor,
};
use crate::presentation::{
    self, ImportErrorKind, ImportFailure, ImportPresentation, parse_network, validate_ip,
    validate_port,
};
use crate::ui::{
    AccountRow, ActivityRow, AppWindow, DiagnosticRow, GuiI18n, ImportBlockedRow,
    ImportDuplicateRow, ImportOptionRow, InstanceRow, JavaRow, LinkedTargetRow, LocalModClashRow,
    LogChunk, ModRow, ModSearchRow, ModVersionRow, PendingEntityRow, PromoteServerRow, RunningRow,
    ServerRow, Theme, WindowSizePreview,
};
use crate::window_aspect;

pub struct Controller {
    _poll_timer: Timer,
    /// A slower poll of its own, so watching what is running does not add work
    /// to every other screen. Runs only while that page is open.
    _running_timer: Timer,
    /// Kept so a later hook, such as [`accept_file_drops`], can send the same
    /// commands the callbacks registered here do.
    commands: Sender<Command>,
    import_flow: Rc<RefCell<ImportFlowState>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum CreationKey {
    Client {
        name: String,
        minecraft: String,
        loader: String,
        loader_version: String,
        persistent: bool,
    },
    Server {
        name: String,
        client: String,
        minecraft: String,
        loader: String,
        loader_version: String,
        persistent: bool,
        port: Option<u16>,
        /// The archive this server's content comes from, empty when it has
        /// none. Part of the key so that picking a different archive is a
        /// different creation rather than a second click on the same one.
        server_pack: String,
    },
    /// One import attempt, identified by the archive it is reading. A second
    /// click on the same archive is the same attempt, not a second one.
    Import { archive: String },
}

#[derive(Debug, Clone)]
struct PendingCreation {
    request_id: String,
    key: CreationKey,
    name: String,
    target: String,
    loader_kind: String,
    storage: String,
    storage_kind: String,
    server: bool,
    linked_client_id: String,
    /// What the work is doing right now, for a creation that has stages worth
    /// watching. Empty for one that finishes before anyone could read it.
    detail: String,
    progress: f32,
    indeterminate: bool,
}

#[derive(Debug, Default)]
struct CreationTracker {
    next_id: u64,
    keys: HashSet<CreationKey>,
    pending: Vec<PendingCreation>,
}

/// One end of a selection: a row, and a byte offset into that row's text.
///
/// Byte offsets rather than columns, because that is what the text item takes
/// and because a column is only a byte offset while a line stays ASCII. The
/// conversion happens once, here, where the row's text is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SelectionPoint {
    row: usize,
    offset: usize,
}

/// What is currently selected across the log's rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LogSelection {
    /// Where the drag began. Stays put while the other end moves.
    anchor: SelectionPoint,
    /// Where the pointer is now.
    focus: SelectionPoint,
}

impl LogSelection {
    /// The two ends in reading order, which is not the order they were made in:
    /// a drag upwards puts the anchor last.
    const fn ordered(&self) -> (SelectionPoint, SelectionPoint) {
        let backwards = self.focus.row < self.anchor.row
            || (self.focus.row == self.anchor.row && self.focus.offset < self.anchor.offset);
        if backwards {
            (self.focus, self.anchor)
        } else {
            (self.anchor, self.focus)
        }
    }

    const fn is_empty(&self) -> bool {
        self.anchor.row == self.focus.row && self.anchor.offset == self.focus.offset
    }
}

struct LiveViewerState {
    active: Option<LiveLogSource>,
    history: RetainedLog,
    chunks: Rc<VecModel<LogChunk>>,
    filter: LogFilter,
    source_truncated: bool,
    pending_lines: usize,
    search_query: String,
    /// What the pointer has selected across rows, if anything.
    selection: Option<LogSelection>,
}

impl Default for LiveViewerState {
    fn default() -> Self {
        Self {
            active: None,
            history: RetainedLog::with_default_limits(),
            chunks: Rc::new(VecModel::default()),
            filter: LogFilter::default(),
            source_truncated: false,
            pending_lines: 0,
            search_query: String::new(),
            selection: None,
        }
    }
}

impl LiveViewerState {
    fn activate(&mut self, source: LiveLogSource) {
        if self.active.as_ref() != Some(&source) {
            self.active = Some(source);
            self.history.clear();
            self.source_truncated = false;
            self.pending_lines = 0;
            self.search_query.clear();
            self.selection = None;
        }
    }

    /// Moves one end of the selection to a character column on a row.
    ///
    /// `extend` keeps the anchor where it is, which is what a shift-click and
    /// the whole of a drag both do; without it the selection starts over at
    /// this point.
    fn select_at(&mut self, row: usize, column: usize, extend: bool) {
        let Some(text) = self.row_text(row) else {
            return;
        };
        // A column past the end of a short line is the end of that line, which
        // is what dragging across a ragged block of text does constantly.
        let offset = text
            .char_indices()
            .nth(column)
            .map_or_else(|| text.len(), |(offset, _)| offset);
        let focus = SelectionPoint { row, offset };
        self.selection = Some(match self.selection {
            Some(existing) if extend => LogSelection {
                anchor: existing.anchor,
                focus,
            },
            _ => LogSelection {
                anchor: focus,
                focus,
            },
        });
    }

    /// Selects the word under a column, the way a double click does.
    ///
    /// "Word" is wider than a natural-language one because a log is not prose:
    /// the things worth grabbing out of one are ids, versions and file names,
    /// so the run continues through the punctuation those are built from. A
    /// UUID, a mod version and a jar name each come out whole.
    fn select_word_at(&mut self, row: usize, column: usize) {
        let Some(text) = self.row_text(row) else {
            return;
        };
        let offset = text
            .char_indices()
            .nth(column)
            .map_or_else(|| text.len(), |(offset, _)| offset);
        let Some((start, end)) = word_bounds(&text, offset) else {
            return;
        };
        self.selection = Some(LogSelection {
            anchor: SelectionPoint { row, offset: start },
            focus: SelectionPoint { row, offset: end },
        });
    }

    /// Selects every row currently shown.
    fn select_all(&mut self) {
        let rows = self.chunks.row_count();
        let Some(last) = rows.checked_sub(1) else {
            self.selection = None;
            return;
        };
        self.selection = Some(LogSelection {
            anchor: SelectionPoint { row: 0, offset: 0 },
            focus: SelectionPoint {
                row: last,
                offset: self.row_text(last).map_or(0, |text| text.len()),
            },
        });
    }

    /// The selected text, joined by newlines, with both ends trimmed to their
    /// own offsets.
    fn selected_text(&self) -> Option<String> {
        let selection = self.selection.filter(|selection| !selection.is_empty())?;
        let (start, end) = selection.ordered();
        let mut lines = Vec::with_capacity(end.row - start.row + 1);
        for row in start.row..=end.row {
            let text = self.row_text(row)?;
            // Clamped rather than sliced blind: the model can be replaced by an
            // arriving line between the drag and the copy, and a stale offset
            // must not be able to panic.
            let from = if row == start.row {
                start.offset.min(text.len())
            } else {
                0
            };
            let to = if row == end.row {
                end.offset.min(text.len())
            } else {
                text.len()
            };
            lines.push(text.get(from..to.max(from)).unwrap_or_default().to_owned());
        }
        Some(lines.join("\n"))
    }

    fn row_text(&self, row: usize) -> Option<String> {
        self.chunks
            .row_data(row)
            .map(|chunk| chunk.text.to_string())
    }

    fn deactivate(&mut self) {
        self.active = None;
        self.pending_lines = 0;
    }

    /// Takes one update, and says what the view has to do about it.
    fn apply(&mut self, update: LiveLogUpdate) -> LiveLogChange {
        if self.active.as_ref() != Some(&update.source) {
            return LiveLogChange::Nothing;
        }
        let reset = update.reset;
        if update.reset {
            self.history.clear();
            self.pending_lines = 0;
            self.source_truncated = false;
        }
        self.source_truncated |= update.source_truncated;
        let added = update.lines.len();
        for line in update.lines {
            match line {
                LiveLogLine::Client(line) => self.history.append_tailed_line(line),
                LiveLogLine::Server(line) => self.history.append_captured(line),
                LiveLogLine::File(line) => self.history.append_line(line),
            }
        }
        self.pending_lines = self.pending_lines.saturating_add(added);
        if reset {
            // A source's whole content, not growth: this is what a file being
            // opened looks like, and it has to be drawn whether or not the
            // view is following. Following decides where a growing log scrolls
            // to; it does not decide whether a log that just arrived is shown.
            return LiveLogChange::Reset;
        }
        if update.missing || update.source_truncated || added > 0 {
            return LiveLogChange::Grew;
        }
        LiveLogChange::Nothing
    }
}

/// What one live-log update means for the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveLogChange {
    /// Nothing to draw: the update was for a source this view is not on, or it
    /// carried no lines.
    Nothing,
    /// The source grew. A view that is following redraws; one the user has
    /// scrolled away from counts the new lines instead.
    Grew,
    /// A source's whole content arrived. Always drawn.
    Reset,
}

impl LiveLogChange {
    /// Whichever of two outcomes asks for more, because one poll can drain
    /// several updates and the view is rendered once for all of them.
    const fn max(self, other: Self) -> Self {
        match (self, other) {
            (Self::Reset, _) | (_, Self::Reset) => Self::Reset,
            (Self::Grew, _) | (_, Self::Grew) => Self::Grew,
            _ => Self::Nothing,
        }
    }
}

fn register_live_viewer(
    ui: &AppWindow,
    commands: &Sender<Command>,
    live_viewer: &Rc<RefCell<LiveViewerState>>,
) {
    ui.set_log_chunks(ModelRc::from(Rc::clone(&live_viewer.borrow().chunks)));

    let sender = commands.clone();
    let weak = ui.as_weak();
    let open_state = Rc::clone(live_viewer);
    ui.on_open_live_log(move |kind, id| {
        let source = match kind.as_str() {
            "client" => backend::parse_instance_id(&id).map(LiveLogSource::Client),
            "server" => backend::parse_server_id(&id).map(LiveLogSource::Server),
            _ => None,
        };
        let Some(source) = source else {
            return;
        };
        open_state.borrow_mut().activate(source.clone());
        if let Some(ui) = weak.upgrade() {
            ui.set_log_query(SharedString::default());
            ui.set_log_follow(true);
            render_live_viewer(&ui, &mut open_state.borrow_mut());
        }
        send(&sender, Command::OpenLiveLog(source));
    });

    let sender = commands.clone();
    let weak = ui.as_weak();
    let file_state = Rc::clone(live_viewer);
    ui.on_open_log_file(move |kind, id, name| {
        let Some(scope) = backend::parse_scope(&kind, &id) else {
            return;
        };
        let source = LiveLogSource::File {
            scope,
            name: name.to_string(),
        };
        file_state.borrow_mut().activate(source.clone());
        if let Some(ui) = weak.upgrade() {
            ui.set_log_query(SharedString::default());
            // A saved log does not grow, so there is nothing to follow. The
            // view still opens at the end, which is where a log is read from.
            ui.set_log_follow(false);
            render_live_viewer(&ui, &mut file_state.borrow_mut());
        }
        send(&sender, Command::OpenLiveLog(source));
    });

    register_log_selection(ui, live_viewer);

    let sender = commands.clone();
    let close_state = Rc::clone(live_viewer);
    ui.on_close_live_log(move || {
        close_state.borrow_mut().deactivate();
        send(&sender, Command::CloseLiveLog);
    });

    let weak = ui.as_weak();
    let filter_state = Rc::clone(live_viewer);
    ui.on_set_log_level(move |level, enabled| {
        let mut state = filter_state.borrow_mut();
        if level == "all" {
            state.filter.set_all(enabled);
        } else if let Some(level) = parse_log_level_name(&level) {
            state.filter.set(level, enabled);
        }
        if let Some(ui) = weak.upgrade() {
            render_live_viewer(&ui, &mut state);
        }
    });

    let weak = ui.as_weak();
    let search_state = Rc::clone(live_viewer);
    ui.on_search_live_log(move |query| {
        let mut state = search_state.borrow_mut();
        state.search_query = query.into();
        if let Some(ui) = weak.upgrade() {
            render_live_viewer(&ui, &mut state);
        }
    });

    let weak = ui.as_weak();
    let follow_state = Rc::clone(live_viewer);
    ui.on_live_log_follow_changed(move |follow| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut state = follow_state.borrow_mut();
        if follow {
            render_live_viewer(&ui, &mut state);
        } else {
            state.pending_lines = 0;
            ui.set_log_pending_lines(0);
        }
    });

    let weak = ui.as_weak();
    let copy_state = Rc::clone(live_viewer);
    ui.on_copy_live_log(move || {
        let content = copy_state.borrow().history.text();
        if let Some(ui) = weak.upgrade() {
            copy_to_clipboard(&ui, content);
        }
    });
}

fn parse_log_level_name(value: &str) -> Option<LogLevel> {
    match value {
        "trace" => Some(LogLevel::Trace),
        "debug" => Some(LogLevel::Debug),
        "info" => Some(LogLevel::Info),
        "warn" => Some(LogLevel::Warn),
        "error" => Some(LogLevel::Error),
        "fatal" => Some(LogLevel::Fatal),
        "other" => Some(LogLevel::Other),
        _ => None,
    }
}

struct LogPresentation {
    chunks: Vec<LogChunk>,
    content_columns: usize,
    match_count: usize,
}

fn build_log_presentation(state: &LiveViewerState) -> LogPresentation {
    const CHUNK_BYTES: usize = 32 * 1024;

    let mut chunks = Vec::new();
    let mut content_columns = 80_usize;
    let visible_records = if state.search_query.is_empty() {
        state
            .history
            .filtered_records(&state.filter)
            .collect::<Vec<_>>()
    } else {
        state.history.search(&state.search_query)
    };
    let match_count = visible_records.len();

    for record in visible_records {
        let mut text = expand_tabs(&record.text);
        if let Some(truncation) = record.truncation {
            text.push('\n');
            text.push_str(&lookup_args(
                "gui-log-bytes-omitted",
                [("count", Arg::from(truncation.omitted_bytes))],
            ));
        }
        content_columns = content_columns.max(
            text.lines()
                .map(|line| line.chars().count())
                .max()
                .unwrap_or(0),
        );
        push_record_chunks(&mut chunks, &text, record.level, CHUNK_BYTES);
    }
    LogPresentation {
        chunks,
        content_columns,
        match_count,
    }
}

/// One row per physical line.
///
/// A record can be a whole stack trace, and a selection that spans rows can
/// only be reasoned about if every row is one line of the same height: the row
/// under the pointer is then the pressed row plus a count of line heights, with
/// no geometry to query. It also keeps the list virtualized over lines rather
/// than over records, so one enormous record does not become one enormous item.
///
/// `max_bytes` still applies, to the single line that exceeds it. A log can
/// contain a line with no newline in it at all, and that is the case the cap
/// exists for.
/// How many columns a tab advances to. Four, like most log viewers.
const TAB_WIDTH: usize = 4;

/// Replaces tabs with the spaces they stand for.
///
/// Two reasons, and either alone would be enough. A tab has no glyph, so a font
/// draws it as the missing-character box -- which is what FML's mod list, two
/// tabs deep on every line, turned into. And the console's selection maps a
/// pointer position to a column by dividing by one character's width, which is
/// only true while every character occupies one column; a tab occupies as many
/// as it takes to reach the next stop.
///
/// Expanded per line, because a tab stop is counted from the start of the line
/// and a record can hold many.
fn expand_tabs(text: &str) -> String {
    if !text.contains('\t') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut column = 0;
    for character in text.chars() {
        match character {
            '\t' => {
                let width = TAB_WIDTH - (column % TAB_WIDTH);
                out.extend(std::iter::repeat_n(' ', width));
                column += width;
            }
            '\n' => {
                out.push('\n');
                column = 0;
            }
            other => {
                out.push(other);
                column += 1;
            }
        }
    }
    out
}

fn push_record_chunks(chunks: &mut Vec<LogChunk>, text: &str, level: LogLevel, max_bytes: usize) {
    for line in text.split('\n') {
        let mut rest = line;
        while rest.len() > max_bytes {
            let mut split = max_bytes;
            while !rest.is_char_boundary(split) {
                split -= 1;
            }
            // A `max_bytes` shorter than one character cannot happen with the
            // cap this is called with, but a zero-length split would loop
            // forever, so it is taken whole instead.
            if split == 0 {
                break;
            }
            let (head, tail) = rest.split_at(split);
            chunks.push(LogChunk {
                text: head.into(),
                level: level.as_str().into(),
            });
            rest = tail;
        }
        chunks.push(LogChunk {
            text: rest.into(),
            level: level.as_str().into(),
        });
    }
}

fn same_chunk(left: &LogChunk, right: &LogChunk) -> bool {
    left.text == right.text && left.level == right.level
}

fn sync_log_chunks(model: &VecModel<LogChunk>, chunks: Vec<LogChunk>) -> bool {
    let current = (0..model.row_count())
        .filter_map(|index| model.row_data(index))
        .collect::<Vec<_>>();
    if current.len() == chunks.len()
        && current
            .iter()
            .zip(&chunks)
            .all(|(left, right)| same_chunk(left, right))
    {
        return false;
    }

    if let Some(first) = chunks.first() {
        for start in 0..current.len() {
            if !same_chunk(&current[start], first) {
                continue;
            }
            let overlap = (current.len() - start).min(chunks.len());
            if current[start..start + overlap]
                .iter()
                .zip(&chunks[..overlap])
                .all(|(left, right)| same_chunk(left, right))
                && overlap == current.len() - start
            {
                for _ in 0..start {
                    model.remove(0);
                }
                for chunk in chunks.into_iter().skip(overlap) {
                    model.push(chunk);
                }
                return true;
            }
        }
    }

    let common_prefix = current
        .iter()
        .zip(&chunks)
        .take_while(|(left, right)| same_chunk(left, right))
        .count();
    while model.row_count() > common_prefix {
        model.remove(model.row_count() - 1);
    }
    for chunk in chunks.into_iter().skip(common_prefix) {
        model.push(chunk);
    }
    true
}

/// Selecting across the console's rows, and copying what was selected.
fn register_log_selection(ui: &AppWindow, live_viewer: &Rc<RefCell<LiveViewerState>>) {
    let weak = ui.as_weak();
    let select_state = Rc::clone(live_viewer);
    ui.on_select_log_at(move |row, column, extend| {
        let (Ok(row), Ok(column)) = (usize::try_from(row), usize::try_from(column)) else {
            return;
        };
        let mut state = select_state.borrow_mut();
        state.select_at(row, column, extend);
        if let Some(ui) = weak.upgrade() {
            publish_log_selection(&ui, &state);
        }
    });

    let weak = ui.as_weak();
    let word_state = Rc::clone(live_viewer);
    ui.on_select_log_word_at(move |row, column| {
        let (Ok(row), Ok(column)) = (usize::try_from(row), usize::try_from(column)) else {
            return;
        };
        let mut state = word_state.borrow_mut();
        state.select_word_at(row, column);
        if let Some(ui) = weak.upgrade() {
            publish_log_selection(&ui, &state);
        }
    });

    let weak = ui.as_weak();
    let all_state = Rc::clone(live_viewer);
    ui.on_select_all_log(move || {
        let mut state = all_state.borrow_mut();
        state.select_all();
        if let Some(ui) = weak.upgrade() {
            publish_log_selection(&ui, &state);
        }
    });

    let weak = ui.as_weak();
    let copy_state = Rc::clone(live_viewer);
    ui.on_copy_log_selection(move || {
        let Some(text) = copy_state.borrow().selected_text() else {
            return;
        };
        if let Some(ui) = weak.upgrade() {
            ui.invoke_copy_text(text.into());
        }
    });
}

/// Whether a character belongs to the middle of a log's idea of a word.
///
/// Alphanumerics plus the punctuation identifiers are built from, so
/// `6d000c93-8762-4090-89d8-097277cd6237`, `26.1.2-7.0.3` and
/// `ftb-library-neoforge-26.1.2.7.jar` are each one word. A path separator is
/// deliberately not one: a double click inside a path should give a segment,
/// not the whole line.
fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '-' | '_' | '.' | '+' | '@' | '#')
}

/// The byte range of the word containing `offset`.
///
/// A click on something that is not a word character takes that one character,
/// which is what makes a double click on a bracket or a space do something
/// small and predictable rather than nothing. `None` when there is nothing
/// under the click at all.
fn word_bounds(text: &str, offset: usize) -> Option<(usize, usize)> {
    // A click past the end belongs to the last character, so double-clicking
    // the empty space after a line still selects its final word.
    let anchor = if offset >= text.len() {
        text.char_indices().next_back().map(|(at, _)| at)?
    } else {
        offset
    };
    let character = text[anchor..].chars().next()?;
    if !is_word_character(character) {
        return Some((anchor, anchor + character.len_utf8()));
    }

    let start = text[..anchor]
        .char_indices()
        .rev()
        .take_while(|(_, character)| is_word_character(*character))
        .last()
        .map_or(anchor, |(at, _)| at);
    let end = text[anchor..]
        .char_indices()
        .take_while(|(_, character)| is_word_character(*character))
        .last()
        .map_or(anchor, |(at, character)| anchor + at + character.len_utf8());
    Some((start, end))
}

/// Puts the selection on screen, in the reading order the rows expect.
///
/// The revision is what makes rows apply it: a selection is a call on a text
/// item rather than a binding, so there has to be something for a row to react
/// to.
fn publish_log_selection(ui: &AppWindow, state: &LiveViewerState) {
    if let Some(selection) = state.selection.filter(|selection| !selection.is_empty()) {
        let (start, end) = selection.ordered();
        ui.set_log_selection_start_row(to_i32(start.row));
        ui.set_log_selection_start_offset(to_i32(start.offset));
        ui.set_log_selection_end_row(to_i32(end.row));
        ui.set_log_selection_end_offset(to_i32(end.offset));
        ui.set_log_has_selection(true);
    } else {
        // A press with no drag is not a selection, and must clear the one it
        // replaced rather than leave it highlighted.
        ui.set_log_selection_start_row(-1);
        ui.set_log_selection_end_row(-1);
        ui.set_log_has_selection(false);
    }
    ui.set_log_selection_revision(ui.get_log_selection_revision().saturating_add(1));
}

fn render_live_viewer(ui: &AppWindow, state: &mut LiveViewerState) {
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
    ui.set_log_has_content(!state.history.is_empty());
    ui.set_log_status(status.into());
    ui.set_log_history_truncated(state.source_truncated || state.history.history_truncated());
    ui.set_log_pending_lines(0);
    ui.set_log_content_columns(to_i32(presentation.content_columns.min(100_000)));
    ui.set_log_trace_enabled(state.filter.is_enabled(LogLevel::Trace));
    ui.set_log_debug_enabled(state.filter.is_enabled(LogLevel::Debug));
    ui.set_log_info_enabled(state.filter.is_enabled(LogLevel::Info));
    ui.set_log_warn_enabled(state.filter.is_enabled(LogLevel::Warn));
    ui.set_log_error_enabled(state.filter.is_enabled(LogLevel::Error));
    ui.set_log_fatal_enabled(state.filter.is_enabled(LogLevel::Fatal));
    ui.set_log_other_enabled(state.filter.is_enabled(LogLevel::Other));
    if chunks_changed {
        ui.set_log_revision(ui.get_log_revision().saturating_add(1));
        // Rows are addressed by index, and the indices just moved. Keeping the
        // highlight would mean highlighting different text than was selected.
        state.selection = None;
        publish_log_selection(ui, state);
    }
    state.pending_lines = 0;
}

/// Pushes the import modal's every visible property from the flow's current
/// state.
///
/// Unconditional and idempotent: called after any state change, including
/// ones (like toggling a checkbox) that only touch one property. `import-name`
/// is the one exception that cannot be blindly re-pushed: see
/// [`ImportFlowState::name_edited`].
fn render_import_flow(ui: &AppWindow, state: &ImportFlowState) {
    ui.set_import_path(state.archive.display().to_string().into());
    if !state.name_edited {
        ui.set_import_name(state.suggested_name.clone().into());
    }
    let status_kind = match state.phase {
        ImportPhase::Idle => "idle",
        ImportPhase::Analyzing => "analyzing",
        ImportPhase::Resolving => "resolving",
        ImportPhase::Ready => "ready",
        ImportPhase::Failed => "failed",
        ImportPhase::Submitting => "submitting",
    };
    ui.set_import_status_kind(status_kind.into());
    ui.set_import_ready(state.is_ready());
    ui.set_modal_error(state.error.clone().into());
    ui.set_import_error_kind(state.error_kind.as_str().into());

    let (format, header, author, minecraft, loader, required, optional) = match &state.presentation
    {
        Some(facts) => (
            facts.format.clone(),
            facts.header_line(),
            facts.author_line(),
            facts.minecraft_line(),
            facts.loader.clone(),
            facts.required_line(),
            facts.optional_line(),
        ),
        None => Default::default(),
    };
    ui.set_import_format(format.into());
    ui.set_import_pack_header(header.into());
    ui.set_import_author_line(author.into());
    ui.set_import_minecraft_line(minecraft.into());
    ui.set_import_loader_line(loader.into());
    ui.set_import_required_line(required.into());
    ui.set_import_optional_line(optional.into());

    let rows: Vec<ImportOptionRow> = state
        .options
        .iter()
        .map(|option| ImportOptionRow {
            logical_path: option.logical_path.clone().into(),
            display: option.display.clone().into(),
            selected: option.selected,
        })
        .collect();
    ui.set_import_optional_items(model(rows));

    let blocked_rows: Vec<ImportBlockedRow> = state
        .blocked
        .iter()
        .map(|blocked| ImportBlockedRow {
            identity: blocked_identity_key(&blocked.identity).into(),
            project_line: blocked.project_line.clone().into(),
            file_line: blocked.file_line.clone().into(),
            browser_url: blocked.browser_url.clone().into(),
        })
        .collect();
    ui.set_import_blocked_items(model(blocked_rows));

    // Purely informational, so unlike `blocked` it has no state of its own:
    // read straight from the current presentation, same as the header lines
    // above it.
    let duplicate_rows: Vec<ImportDuplicateRow> = state
        .presentation
        .as_ref()
        .map(|facts| facts.duplicate_files.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|duplicate| ImportDuplicateRow {
            project_line: duplicate.project_line.clone().into(),
            kept_line: duplicate.kept_line.clone().into(),
            discarded_line: duplicate.discarded_line.clone().into(),
            matched_minecraft_version: duplicate.matched_minecraft_version,
        })
        .collect();
    ui.set_import_duplicate_items(model(duplicate_rows));
}

fn register_import_flow(
    ui: &AppWindow,
    commands: &Sender<Command>,
    import_flow: &Rc<RefCell<ImportFlowState>>,
    creations: &Rc<RefCell<CreationTracker>>,
) {
    let submit_creations = Rc::clone(creations);
    let sender = commands.clone();
    let weak = ui.as_weak();
    let pick_state = Rc::clone(import_flow);
    ui.on_choose_import_file(move || {
        // The attempt begins here, synchronously, before the native dialog
        // even opens: `Analyzing` must be visible immediately, and any
        // result for whatever attempt was current before this one has to
        // fail its generation check the moment it arrives, not once the
        // dialog happens to close.
        let generation = pick_state.borrow_mut().begin_attempt();
        if let Some(ui) = weak.upgrade() {
            set_server_pack(&ui, ServerPackTarget::Import, None);
            render_import_flow(&ui, &pick_state.borrow());
        }
        send(&sender, Command::PickImport { generation });
    });

    let weak = ui.as_weak();
    let open_state = Rc::clone(import_flow);
    ui.on_open_import_modal(move || {
        open_state.borrow_mut().reset();
        if let Some(ui) = weak.upgrade() {
            set_server_pack(&ui, ServerPackTarget::Import, None);
            render_import_flow(&ui, &open_state.borrow());
            ui.set_modal_error(SharedString::default());
            ui.set_modal("import".into());
        }
    });

    let sender = commands.clone();
    let weak = ui.as_weak();
    let close_state = Rc::clone(import_flow);
    ui.on_close_import_modal(move || {
        close_state.borrow_mut().reset();
        // Abandoning the archive is not asking the backend to cancel a
        // user-visible operation, so this bypasses the toast-producing
        // `CancelOperation` path and goes straight to the worker.
        send(&sender, Command::CancelImportAnalysis);
        if let Some(ui) = weak.upgrade() {
            set_server_pack(&ui, ServerPackTarget::Import, None);
            render_import_flow(&ui, &close_state.borrow());
            ui.set_modal(SharedString::default());
            ui.set_modal_error(SharedString::default());
        }
    });

    let weak = ui.as_weak();
    let toggle_state = Rc::clone(import_flow);
    ui.on_toggle_import_optional(move |logical_path| {
        toggle_state
            .borrow_mut()
            .toggle_option(logical_path.as_str());
        if let Some(ui) = weak.upgrade() {
            render_import_flow(&ui, &toggle_state.borrow());
        }
    });

    let edited_state = Rc::clone(import_flow);
    ui.on_import_name_edited(move |_text| {
        edited_state.borrow_mut().mark_name_edited();
    });

    register_blocked_file_actions(ui, import_flow);

    let sender = commands.clone();
    let weak = ui.as_weak();
    let submit_state = Rc::clone(import_flow);
    ui.on_submit_import(move |name, persistent, create_server, server_pack| {
        // The archive comes from the flow's own state, never from a UI
        // field: `import-path` only ever displays what the last successful
        // analysis found, and is not itself a source of a new one.
        let submission = {
            let mut state = submit_state.borrow_mut();
            let Some(submission) = state.begin_submit() else {
                // Nothing is ready, or a submission for this archive is
                // already running: a second click must not start a second
                // import.
                return;
            };
            submission
        };
        // The dialog has nothing left to ask, and the work that follows can
        // take minutes. It closes now, and the instances list carries the
        // import from here as a card that says what it is doing.
        let key = CreationKey::Import {
            archive: submission.archive.display().to_string(),
        };
        let request_id = submit_creations
            .borrow_mut()
            .accept(key.clone(), pending_creation(&key, persistent));
        if let Some(ui) = weak.upgrade() {
            if request_id.is_some() {
                ui.set_modal(SharedString::default());
                ui.set_modal_error(SharedString::default());
            }
            apply_pending_creations(&ui, &submit_creations.borrow());
            render_import_flow(&ui, &submit_state.borrow());
        }
        send(
            &sender,
            Command::ImportPack {
                archive: submission.archive,
                name: name.into(),
                persistent,
                selection: submission.selection,
                skip_blocked: submission.skip_blocked,
                generation: submission.generation,
                request_id,
                create_linked_server: create_server,
                server_pack: (create_server && !server_pack.is_empty())
                    .then(|| PathBuf::from(server_pack.as_str())),
            },
        );
    });
}

/// Wires the import modal's blocked-file list: skipping one, and opening its
/// project page in the user's browser.
fn register_blocked_file_actions(ui: &AppWindow, import_flow: &Rc<RefCell<ImportFlowState>>) {
    let weak = ui.as_weak();
    let skip_state = Rc::clone(import_flow);
    ui.on_skip_blocked_file(move |key| {
        let mut state = skip_state.borrow_mut();
        let Some(identity) = state
            .blocked
            .iter()
            .find(|blocked| blocked_identity_key(&blocked.identity) == key.as_str())
            .map(|blocked| blocked.identity.clone())
        else {
            return;
        };
        state.skip_blocked_file(&identity);
        drop(state);
        if let Some(ui) = weak.upgrade() {
            render_import_flow(&ui, &skip_state.borrow());
        }
    });

    let weak = ui.as_weak();
    ui.on_open_blocked_file(move |url| {
        if url.is_empty() {
            return;
        }
        let opened = ephervex_core::open_in_browser(url.as_str());
        if let Some(ui) = weak.upgrade() {
            ui.invoke_present_toast(
                lookup(if opened {
                    "gui-import-blocked-opened"
                } else {
                    "gui-import-blocked-open-failed"
                })
                .into(),
                !opened,
            );
        }
    });
}

impl CreationTracker {
    fn accept(&mut self, key: CreationKey, mut pending: PendingCreation) -> Option<String> {
        if !self.keys.insert(key.clone()) {
            return None;
        }
        self.next_id = self.next_id.saturating_add(1);
        let request_id = format!("creation-{}", self.next_id);
        pending.request_id.clone_from(&request_id);
        pending.key = key;
        self.pending.push(pending);
        Some(request_id)
    }

    /// Records what a running creation is doing, for the card that shows it.
    fn progressed(&mut self, request_id: &str, detail: String, progress: f32, indeterminate: bool) {
        if let Some(pending) = self
            .pending
            .iter_mut()
            .find(|pending| pending.request_id == request_id)
        {
            pending.detail = detail;
            pending.progress = progress;
            pending.indeterminate = indeterminate;
        }
    }

    fn finish(&mut self, request_id: &str) {
        if let Some(index) = self
            .pending
            .iter()
            .position(|pending| pending.request_id == request_id)
        {
            let pending = self.pending.remove(index);
            self.keys.remove(&pending.key);
        }
    }
}

/// Where one archive's import attempt stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ImportPhase {
    /// No archive chosen, or the modal was closed before one finished.
    #[default]
    Idle,
    /// Local analysis is running. Fast and inline in the backend, but the
    /// modal still needs an explicit state for it: readiness must not flash
    /// true before it exists.
    Analyzing,
    /// A provider-backed pack is being resolved over the network.
    Resolving,
    /// Fully analyzed (and resolved, if it needed to be) and importable.
    Ready,
    /// Local analysis or provider resolution failed. Recoverable only by
    /// choosing another archive.
    Failed,
    /// An `ImportPack` submission is in flight for the current archive.
    Submitting,
}

/// One mod the browser was asked to install, by the identity its plan and its
/// completion both come back under.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserInstall {
    instance: String,
    provider: String,
    project: String,
}

/// The browser's install requests, in the order they were pressed.
///
/// One at a time, and that is not a presentation choice: installing reads the
/// instance's recipe, adds to it, and writes it back, so two installs running
/// against one instance would lose one of the two updates. Queuing is what lets
/// every Install button stay live while only one install is under way, instead
/// of a second press replacing the first request and quietly discarding it.
#[derive(Debug, Default)]
struct BrowserInstallQueue {
    /// The request whose plan is being awaited, is being confirmed, or is
    /// installing. Nothing else starts while this is set.
    active: Option<BrowserInstall>,
    waiting: std::collections::VecDeque<BrowserInstall>,
}

impl BrowserInstallQueue {
    /// Adds a request, and hands back the one to start now if nothing is
    /// running. A request already queued or already running is ignored: two
    /// presses on one row mean the same single install.
    fn enqueue(&mut self, request: BrowserInstall) -> Option<BrowserInstall> {
        if self.active.as_ref() == Some(&request) || self.waiting.contains(&request) {
            return None;
        }
        if self.active.is_some() {
            self.waiting.push_back(request);
            return None;
        }
        self.active = Some(request.clone());
        Some(request)
    }

    /// Retires the running request, if `request` is it, and hands back the one
    /// to start next.
    ///
    /// A report for anything else is ignored rather than trusted: an install
    /// the user started from the managed-mods side finishes through the same
    /// event and must not advance this queue.
    fn finish(&mut self, request: &BrowserInstall) -> Option<BrowserInstall> {
        if self.active.as_ref() != Some(request) {
            return None;
        }
        self.active = self.waiting.pop_front();
        self.active.clone()
    }

    /// Retires whatever is running, whether or not its identity is known.
    ///
    /// For the endings that arrive without one, such as a confirmation the user
    /// closed.
    fn finish_active(&mut self) -> Option<BrowserInstall> {
        self.active.take()?;
        self.active = self.waiting.pop_front();
        self.active.clone()
    }

    /// Every project this queue still owes an install, running one included.
    fn outstanding(&self, instance: &str, provider: &str) -> Vec<&str> {
        self.active
            .iter()
            .chain(self.waiting.iter())
            .filter(|request| request.instance == instance && request.provider == provider)
            .map(|request| request.project.as_str())
            .collect()
    }
}

/// The mod browser's own bookkeeping, kept together because a chained fill
/// needs both halves: the queue decides what may install next, and the counter
/// hands every request in a fill its own identity.
#[derive(Debug, Default)]
struct ModBrowser {
    installs: RefCell<BrowserInstallQueue>,
    search_requests: Cell<u64>,
}

/// One optional component's checkbox state in the import modal.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ImportOptionState {
    logical_path: String,
    display: String,
    selected: bool,
}

/// One still-blocked file's state in the import modal, pending a watcher
/// match or a skip.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockedFileState {
    identity: ProviderFileIdentity,
    project_line: String,
    file_line: String,
    browser_url: String,
}

/// Everything `Command::ImportPack` needs, built from [`ImportFlowState`]'s
/// own authoritative fields by [`ImportFlowState::begin_submit`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedImportSubmission {
    generation: u64,
    archive: PathBuf,
    selection: OptionalSelection,
    skip_blocked: Vec<ProviderFileIdentity>,
}

/// Tracks one archive at a time through analysis, optional resolution,
/// optional-file selection, and submission.
///
/// The identity that makes a late result discardable is `tracked_generation`,
/// not the archive path: two picks of the same file are two different
/// attempts, and a stale one must not win just because the name matches.
/// `next_generation` is the only field [`Self::reset`] must not clear: it
/// has to keep counting up across modal close/reopen, or a later attempt
/// could be assigned a number an earlier, already-abandoned one used, which
/// would let that earlier attempt's late result pass the staleness check.
#[derive(Debug, Default)]
struct ImportFlowState {
    next_generation: u64,
    tracked_generation: Option<u64>,
    phase: ImportPhase,
    archive: PathBuf,
    presentation: Option<ImportPresentation>,
    options: Vec<ImportOptionState>,
    /// Files a provider refused that neither the shared cache nor an
    /// explicit skip has cleared yet. Import stays disabled while this is
    /// non-empty; see [`Self::is_ready`].
    blocked: Vec<BlockedFileState>,
    /// Blocked files the user has explicitly chosen to do without, threaded
    /// into `Command::ImportPack` at submit time. Also used to filter a
    /// freshly arrived `blocked` list, so a decision already made does not
    /// reappear if resolution runs again.
    skipped: Vec<ProviderFileIdentity>,
    suggested_name: String,
    /// Set the moment the user types in the name field. Checked before a
    /// later analysis/resolution result would otherwise overwrite it: an
    /// edit survives everything except a genuinely new archive pick.
    name_edited: bool,
    error: String,
    /// The category `error` was classified from, so the modal can offer a
    /// state-specific affordance (like opening Providers & APIs for a
    /// credential problem) without ever pattern-matching `error`'s text.
    error_kind: ImportErrorKind,
}

impl ImportFlowState {
    fn reset(&mut self) {
        let next_generation = self.next_generation;
        *self = Self::default();
        self.next_generation = next_generation;
    }

    /// Starts a new attempt and returns the generation to send with
    /// [`Command::PickImport`].
    ///
    /// Called the instant the user asks to choose an archive, before the
    /// native file dialog even opens: entering `Analyzing` here, rather than
    /// waiting for a result to arrive, is what makes the phase real instead
    /// of a state nothing ever assigns. It also invalidates whatever attempt
    /// was current before it, the same way closing the modal does, so a
    /// result for that older attempt is rejected by every method below
    /// regardless of how the new one turns out.
    fn begin_attempt(&mut self) -> u64 {
        self.reset();
        self.next_generation = self.next_generation.saturating_add(1);
        self.tracked_generation = Some(self.next_generation);
        self.phase = ImportPhase::Analyzing;
        self.next_generation
    }

    /// The native file dialog closed with no choice made. `false` means it
    /// was discarded: a newer attempt is already current. Otherwise the
    /// attempt returns to `Idle`, the same safe, non-ready state as before
    /// the user ever asked to choose an archive.
    fn pick_canceled(&mut self, generation: u64) -> bool {
        if self.tracked_generation != Some(generation) {
            return false;
        }
        self.reset();
        true
    }

    /// Applies a local-analysis result for a freshly chosen archive.
    ///
    /// `false` means it was discarded: `generation` no longer names the
    /// attempt this flow is tracking, because a newer archive superseded it
    /// or the modal was closed before this arrived. This is also the one
    /// point a new archive's suggested name and edited-flag are reset, which
    /// is what keeps the name deterministic per archive rather than carrying
    /// a previous pack's edit forward.
    fn analyzed(
        &mut self,
        generation: u64,
        archive: PathBuf,
        result: Result<ImportPresentation, ImportFailure>,
    ) -> bool {
        if self.tracked_generation != Some(generation) {
            return false;
        }
        self.archive = archive;
        self.options.clear();
        self.error.clear();
        self.error_kind = ImportErrorKind::default();
        self.name_edited = false;
        self.suggested_name.clear();
        match result {
            Ok(presentation) => {
                self.suggested_name.clone_from(&presentation.name);
                self.options = options_from(&presentation);
                self.blocked = blocked_from(&presentation, &self.skipped);
                self.phase = if presentation.needs_provider_resolution {
                    ImportPhase::Resolving
                } else {
                    ImportPhase::Ready
                };
                self.presentation = Some(presentation);
            }
            Err(failure) => {
                self.presentation = None;
                self.error = failure.message;
                self.error_kind = failure.kind;
                self.phase = ImportPhase::Failed;
            }
        }
        true
    }

    /// Applies a provider-resolution result.
    ///
    /// `false` means it was discarded: `generation` no longer names the
    /// attempt this flow is tracking, because a newer archive superseded it
    /// or the modal was closed. The caller has nothing further to render in
    /// that case.
    fn resolved(
        &mut self,
        generation: u64,
        result: Result<ImportPresentation, ImportFailure>,
    ) -> bool {
        if self.tracked_generation != Some(generation) {
            return false;
        }
        match result {
            Ok(presentation) => {
                if !self.name_edited {
                    self.suggested_name.clone_from(&presentation.name);
                }
                self.options = options_from(&presentation);
                self.blocked = blocked_from(&presentation, &self.skipped);
                self.phase = ImportPhase::Ready;
                self.presentation = Some(presentation);
            }
            Err(failure) => {
                self.presentation = None;
                self.error = failure.message;
                self.error_kind = failure.kind;
                self.phase = ImportPhase::Failed;
            }
        }
        true
    }

    fn mark_name_edited(&mut self) {
        self.name_edited = true;
    }

    fn toggle_option(&mut self, logical_path: &str) {
        if let Some(option) = self
            .options
            .iter_mut()
            .find(|option| option.logical_path == logical_path)
        {
            option.selected = !option.selected;
        }
    }

    /// Removes a blocked file from the modal's list and records the skip, so
    /// final import omits it and the created instance is flagged with
    /// `AttentionReason::ManualDownloadRequired` for it instead.
    fn skip_blocked_file(&mut self, identity: &ProviderFileIdentity) {
        self.blocked.retain(|blocked| &blocked.identity != identity);
        if !self.skipped.contains(identity) {
            self.skipped.push(identity.clone());
        }
    }

    /// A folder watcher found and verified a copy of `identity`'s file.
    /// `false` means it was discarded: either `generation` no longer names
    /// the attempt this flow is tracking, or the row was already gone (a
    /// stale or duplicate event) - either way there is nothing new to
    /// render.
    fn blocked_file_matched(&mut self, generation: u64, identity: &ProviderFileIdentity) -> bool {
        if self.tracked_generation != Some(generation) {
            return false;
        }
        let before = self.blocked.len();
        self.blocked.retain(|blocked| &blocked.identity != identity);
        self.blocked.len() != before
    }

    /// Ready to submit only once analysis (and resolution, if it needed to
    /// run) has finished *and* nothing a provider refused is still
    /// unaccounted for: [`Self::skip_blocked_file`] or a watcher match
    /// clears each one, or the pack simply had none.
    fn is_ready(&self) -> bool {
        matches!(self.phase, ImportPhase::Ready) && self.blocked.is_empty()
    }

    /// Tries to move into `Submitting`, returning everything `ImportPack`
    /// needs, drawn only from this flow's own state. `None` means the caller
    /// must not send anything: either nothing is ready yet, or a submission
    /// for this same archive is already running.
    ///
    /// The archive here, not whatever the UI's editable text field currently
    /// shows, is what makes it into the command: the field can only ever
    /// reflect a completed analysis, never drive one, so trusting it at
    /// submit time would let a manually typed path escape analysis and
    /// optional-selection entirely.
    fn begin_submit(&mut self) -> Option<PreparedImportSubmission> {
        if !self.is_ready() {
            return None;
        }
        let generation = self.tracked_generation?;
        self.phase = ImportPhase::Submitting;
        Some(PreparedImportSubmission {
            generation,
            archive: self.archive.clone(),
            selection: self.selection(),
            skip_blocked: self.skipped.clone(),
        })
    }

    /// The submission for `generation` reached a terminal outcome, however
    /// it finished. `false` means it was ignored: either `generation` no
    /// longer names the attempt this flow is tracking (a newer attempt
    /// started, or the modal was closed, while the submission was still in
    /// flight), or nothing was actually submitting. This is the only way the
    /// double-submit guard is released: a generic event with no submission
    /// identity of its own cannot call this at all.
    fn submit_finished(&mut self, generation: u64) -> bool {
        if self.tracked_generation != Some(generation)
            || !matches!(self.phase, ImportPhase::Submitting)
        {
            return false;
        }
        self.phase = ImportPhase::Ready;
        true
    }

    /// The exact `OptionalSelection` the current checkbox state describes.
    fn selection(&self) -> OptionalSelection {
        OptionalSelection::of(
            self.options
                .iter()
                .filter(|option| option.selected)
                .map(|option| option.logical_path.clone()),
        )
    }
}

fn options_from(presentation: &ImportPresentation) -> Vec<ImportOptionState> {
    presentation
        .optional
        .iter()
        .map(|option| ImportOptionState {
            logical_path: option.logical_path.clone(),
            display: option.display.clone(),
            selected: option.selected,
        })
        .collect()
}

/// The opaque join key a blocked row's `identity` Slint field carries.
/// `skip-blocked-file` calls back with exactly this string, and the handler
/// matches it back against `ImportFlowState::blocked` rather than parsing it,
/// so the two directions can never disagree about what it means.
fn blocked_identity_key(identity: &ProviderFileIdentity) -> String {
    format!(
        "{}/{}",
        identity.project_id.as_deref().unwrap_or_default(),
        identity.file_id.as_deref().unwrap_or_default(),
    )
}

/// Everything a freshly arrived analysis/resolution result reports as
/// blocked, minus whatever the user already decided to skip: a decision
/// already made must not reappear just because resolution answered again.
fn blocked_from(
    presentation: &ImportPresentation,
    skipped: &[ProviderFileIdentity],
) -> Vec<BlockedFileState> {
    presentation
        .blocked
        .iter()
        .filter(|blocked| !skipped.contains(&blocked.identity))
        .map(|blocked| BlockedFileState {
            identity: blocked.identity.clone(),
            project_line: blocked.project_line.clone(),
            file_line: blocked.file_line.clone(),
            browser_url: blocked.browser_url.clone(),
        })
        .collect()
}

impl std::fmt::Debug for Controller {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Controller").finish_non_exhaustive()
    }
}

/// What a file let go on the window is asking for.
///
/// Winit reports a drop with no cursor position, so a drop cannot be aimed at a
/// field the way a click is. What the window is currently asking for decides
/// instead, and that is legible on screen: a dialog showing an empty archive
/// slot is asking for that archive.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DropIntent {
    /// Import it as a pack. The default, and what a drop with nothing open
    /// means.
    Import,
    /// Fill one of the dialogs' server-archive fields.
    ServerPack(ServerPackTarget),
    /// Add the jars to this host's mods folder.
    ///
    /// What an open mods tab is asking for, in the same way an empty archive
    /// slot asks for an archive. A drop has no cursor position, so the tab
    /// being open is the whole aim.
    Mods(ModHost),
    /// Nothing. A file let go over a confirmation the user is in the middle of
    /// is far more likely to be a misplaced gesture than a request to abandon
    /// it.
    Ignored,
}

/// Which field, if any, a drop belongs to right now.
fn drop_intent(ui: &AppWindow) -> DropIntent {
    match ui.get_modal().as_str() {
        "create-server" => DropIntent::ServerPack(ServerPackTarget::NewServer),
        // The import dialog asks for two archives once a server is wanted, and
        // the pack itself is already chosen by the time that toggle can be
        // reached. So a drop there names the server files.
        "import" if ui.get_import_create_server() => {
            DropIntent::ServerPack(ServerPackTarget::Import)
        }
        // Before the import arm below, which is what an empty modal string
        // otherwise means: a jar let go over a visible mod list is not a pack
        // somebody wants imported.
        "" => mods_drop_intent(ui).unwrap_or(DropIntent::Import),
        "import" => DropIntent::Import,
        _ => DropIntent::Ignored,
    }
}

/// The mod host whose list is on screen, if one is.
fn mods_drop_intent(ui: &AppWindow) -> Option<DropIntent> {
    if ui.get_workspace_tab() != "Mods" {
        return None;
    }
    let host = match ui.get_workspace().as_str() {
        "instance" => backend::parse_mod_host("client", &ui.get_selected_instance_id())?,
        "server" => backend::parse_mod_host("server", &ui.get_selected_server_id())?,
        _ => return None,
    };
    Some(DropIntent::Mods(host))
}

/// Routes files dropped on the window to whatever is asking for them.
///
/// For an import the drop is the same act as choosing a file, so it takes the
/// same path: the attempt begins here, synchronously, and the analysis that
/// follows is checked against it exactly as a dialog's would be. Opening the
/// dialog is part of accepting the drop, because a drop with nothing on screen
/// would look like nothing happened.
///
/// A drop carries every path of one gesture. Only the mods list has anything to
/// do with more than one: an archive field holds a single file, so it takes the
/// first and leaves the rest, which is what dropping a handful of archives onto
/// one slot can reasonably mean.
pub fn accept_file_drops(ui: &AppWindow, drops: &FileDrops, controller: &Controller) {
    let weak = ui.as_weak();
    let sender = controller.commands.clone();
    let flow = Rc::clone(&controller.import_flow);
    drops.listen(move |drop| {
        let Some(ui) = weak.upgrade() else { return };
        let intent = drop_intent(&ui);
        match drop {
            FileDrop::Hovering => {
                ui.set_file_drop_active(intent != DropIntent::Ignored);
                ui.set_file_drop_kind(drop_overlay_kind(&intent).into());
            }
            FileDrop::Left => ui.set_file_drop_active(false),
            FileDrop::Dropped(paths) => {
                ui.set_file_drop_active(false);
                match intent {
                    DropIntent::Ignored => {}
                    DropIntent::Mods(host) => {
                        send(
                            &sender,
                            Command::PlanLocalMods {
                                host,
                                sources: paths,
                            },
                        );
                    }
                    DropIntent::ServerPack(target) => {
                        set_server_pack(&ui, target, paths.first().map(PathBuf::as_path));
                    }
                    DropIntent::Import => {
                        let Some(archive) = paths.into_iter().next() else {
                            return;
                        };
                        begin_import(&ui, &flow, &sender, archive);
                    }
                }
            }
        }
    });
}

/// Opens the import dialog on one archive and starts reading it.
///
/// The same sequence however the archive arrived: let go over the window, or
/// downloaded because a web page offered a link. A link-initiated import is a
/// drop with a download in front of it, so its progress belongs in the dialog
/// the user already knows rather than in one of its own.
fn begin_import(
    ui: &AppWindow,
    flow: &Rc<RefCell<ImportFlowState>>,
    commands: &Sender<Command>,
    archive: PathBuf,
) {
    let generation = flow.borrow_mut().begin_attempt();
    set_server_pack(ui, ServerPackTarget::Import, None);
    render_import_flow(ui, &flow.borrow());
    ui.set_modal_error(SharedString::default());
    ui.set_modal("import".into());
    send(
        commands,
        Command::AnalyzeImport {
            generation,
            archive,
        },
    );
}

/// Which promise the drop overlay should make, for what is on screen.
const fn drop_overlay_kind(intent: &DropIntent) -> &'static str {
    match intent {
        DropIntent::Mods(_) => "mods",
        _ => "import",
    }
}

pub fn attach(ui: &AppWindow) -> Controller {
    let backend = backend::spawn();
    let receiver = Rc::new(RefCell::new(backend.events));
    let activities = Rc::new(RefCell::new(Vec::<ActivityData>::new()));
    let creations = Rc::new(RefCell::new(CreationTracker::default()));
    let live_viewer = Rc::new(RefCell::new(LiveViewerState::default()));
    let import_flow = Rc::new(RefCell::new(ImportFlowState::default()));
    let minecraft = Rc::new(RefCell::new(MinecraftDefaultsModel::new()));
    let browser = Rc::new(ModBrowser::default());
    let mod_lists = Rc::new(RefCell::new(ModLists::default()));
    let local_mods = Rc::new(RefCell::new(LocalModDrop::default()));
    register_i18n(ui);
    register_toasts(ui);
    register_callbacks(
        ui,
        &backend.commands,
        &creations,
        &live_viewer,
        &import_flow,
        &minecraft,
        &browser,
        &mod_lists,
        &local_mods,
    );

    let weak = ui.as_weak();
    let poll_receiver = Rc::clone(&receiver);
    let poll_activities = Rc::clone(&activities);
    let poll_creations = Rc::clone(&creations);
    let poll_live_viewer = Rc::clone(&live_viewer);
    let poll_import_flow = Rc::clone(&import_flow);
    let poll_minecraft = Rc::clone(&minecraft);
    let poll_browser = Rc::clone(&browser);
    let poll_mod_lists = Rc::clone(&mod_lists);
    let poll_local_mods = Rc::clone(&local_mods);
    let poll_commands = backend.commands.clone();
    let poll_timer = Timer::default();
    poll_timer.start(TimerMode::Repeated, Duration::from_millis(80), move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let window = ui.window();
        let logical_width = f64::from(window.size().width) / f64::from(window.scale_factor());
        let show_inspector = logical_width >= 1180.0;
        let compact_layout = !show_inspector;
        if ui.get_show_inspector() != show_inspector {
            ui.set_show_inspector(show_inspector);
        }
        if ui.get_compact_layout() != compact_layout {
            ui.set_compact_layout(compact_layout);
        }
        let mut live_change = LiveLogChange::Nothing;
        for event in poll_receiver.borrow_mut().try_iter() {
            match event {
                Event::LiveLog(update) => {
                    live_change = live_change.max(poll_live_viewer.borrow_mut().apply(update));
                }
                other => apply_event(
                    &ui,
                    other,
                    &poll_activities,
                    &poll_creations,
                    &poll_import_flow,
                    &poll_minecraft,
                    &poll_browser,
                    &poll_mod_lists,
                    &poll_local_mods,
                    &poll_commands,
                ),
            }
        }
        match live_change {
            // A saved log is opened with following off, because a finished log
            // has nothing to follow. Counting its lines instead of drawing them
            // left the view empty until it was opened a second time.
            LiveLogChange::Reset => {
                render_live_viewer(&ui, &mut poll_live_viewer.borrow_mut());
            }
            LiveLogChange::Grew if ui.get_log_follow() => {
                render_live_viewer(&ui, &mut poll_live_viewer.borrow_mut());
            }
            LiveLogChange::Grew => {
                ui.set_log_pending_lines(to_i32(poll_live_viewer.borrow().pending_lines));
            }
            LiveLogChange::Nothing => {}
        }
    });

    // A second is enough for uptime and for CPU to read smoothly, and it costs
    // nothing while the page is closed, which is nearly always.
    let running_weak = ui.as_weak();
    let running_commands = backend.commands.clone();
    let running_timer = Timer::default();
    running_timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
        let Some(ui) = running_weak.upgrade() else {
            return;
        };
        if ui.get_page() == RUNNING_PAGE && ui.get_workspace().is_empty() {
            send(&running_commands, Command::LoadRunning);
        }
    });

    Controller {
        _poll_timer: poll_timer,
        _running_timer: running_timer,
        commands: backend.commands,
        import_flow,
    }
}

/// The navigation entry that lists client instances.
///
/// Both workspaces hang off a page, so navigating to one means setting the page
/// as well. Named for the same reason as the others: a bare number in that
/// assignment would say nothing about where the user is being sent.
const INSTANCES_PAGE: i32 = 0;

/// The navigation entry that lists servers, and hosts the server workspace.
const SERVERS_PAGE: i32 = 1;

/// The navigation entry that lists what is running.
///
/// Named because the poll that feeds it has to know when it is on screen, and a
/// bare number in that check would say nothing about which screen it is.
const RUNNING_PAGE: i32 = 2;

fn register_toasts(ui: &AppWindow) {
    const MAX_QUEUED_TOASTS: usize = 12;

    let queue = Rc::new(RefCell::new(VecDeque::<(SharedString, bool)>::new()));
    let queued = Rc::clone(&queue);
    ui.on_queue_toast(move |message, error| {
        let mut queued = queued.borrow_mut();
        if queued.len() == MAX_QUEUED_TOASTS {
            queued.pop_front();
        }
        queued.push_back((message, error));
    });

    let weak = ui.as_weak();
    ui.on_toast_slot_available(move || {
        let next = queue.borrow_mut().pop_front();
        if let (Some((message, error)), Some(ui)) = (next, weak.upgrade()) {
            ui.invoke_present_toast(message, error);
        }
    });
}

/// Every window gets its own copy of the translation globals, so each one is
/// given the same lookups rather than rendering its message ids.
fn register_i18n<'a, T: slint::ComponentHandle + 'a>(ui: &'a T)
where
    GuiI18n<'a>: slint::Global<'a, T>,
{
    let i18n = ui.global::<GuiI18n>();
    i18n.on_text(|id| lookup(id.as_str()).into());
    i18n.on_count(|id, count| lookup_args(id.as_str(), [("count", Arg::from(count))]).into());
    i18n.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.as_str()))]).into()
    });
    i18n.on_values(|id, first, second| {
        lookup_args(
            id.as_str(),
            [
                ("first", Arg::from(first.as_str())),
                ("second", Arg::from(second.as_str())),
            ],
        )
        .into()
    });
}

/// Submitting the create-server dialog.
fn register_create_server(
    ui: &AppWindow,
    commands: &Sender<Command>,
    creations: Rc<RefCell<CreationTracker>>,
) {
    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_submit_create_server(
        move |name,
              client,
              minecraft,
              loader,
              loader_version,
              persistent,
              automatic_port,
              port,
              server_pack| {
            let linked = !client.is_empty();
            let port = if automatic_port {
                None
            } else {
                let validation = validate_port(&port);
                if !validation.valid {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_modal_error(validation.message.into());
                    }
                    return;
                }
                port.parse::<u16>().ok()
            };
            let key = CreationKey::Server {
                name: name.trim().to_owned(),
                client: client.as_str().to_owned(),
                minecraft: if linked {
                    String::new()
                } else {
                    minecraft.as_str().to_owned()
                },
                loader: if linked {
                    String::new()
                } else {
                    loader.as_str().to_owned()
                },
                loader_version: if linked {
                    String::new()
                } else {
                    loader_version.as_str().to_owned()
                },
                persistent,
                port,
                server_pack: server_pack.as_str().to_owned(),
            };
            let pending = pending_creation(&key, false);
            let Some(request_id) = creations.borrow_mut().accept(key, pending) else {
                if let Some(ui) = weak.upgrade() {
                    ui.invoke_present_toast(lookup("gui-creation-already-running").into(), true);
                }
                return;
            };
            if let Some(ui) = weak.upgrade() {
                ui.set_modal(SharedString::default());
                ui.set_modal_error(SharedString::default());
                apply_pending_creations(&ui, &creations.borrow());
            }
            let client = if client.is_empty() {
                None
            } else {
                backend::parse_instance_id(&client)
            };
            send(
                &sender,
                Command::CreateServer {
                    request_id,
                    name: name.into(),
                    client,
                    minecraft: minecraft.into(),
                    loader: loader.into(),
                    loader_version: loader_version.into(),
                    persistent,
                    port,
                    server_pack: (!server_pack.is_empty())
                        .then(|| PathBuf::from(server_pack.as_str())),
                },
            );
        },
    );
}

/// Narrowing a mod list to what the user typed.
///
/// The full list stays in `mod_lists`, so clearing the box restores it without
/// asking core for the mods again.
fn register_mod_filter(ui: &AppWindow, mod_lists: &Rc<RefCell<ModLists>>) {
    let weak = ui.as_weak();
    let lists = Rc::clone(mod_lists);
    ui.on_filter_mods(move |host_kind, _query| {
        if let Some(ui) = weak.upgrade() {
            render_mod_list(&ui, &lists.borrow(), host_kind.as_str());
        }
    });
}

/// Choosing, and unchoosing, the archive a server's content comes from.
///
/// Two dialogs can name one, and the pick is asynchronous, so each request says
/// where its answer belongs rather than the reply guessing from whatever modal
/// happens to be open when it lands.
fn register_server_pack_choice(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    ui.on_choose_server_pack(move || {
        send(
            &sender,
            Command::PickServerPack {
                target: ServerPackTarget::NewServer,
            },
        );
    });

    let sender = commands.clone();
    ui.on_choose_import_server_pack(move || {
        send(
            &sender,
            Command::PickServerPack {
                target: ServerPackTarget::Import,
            },
        );
    });

    let weak = ui.as_weak();
    ui.on_clear_server_pack(move || {
        if let Some(ui) = weak.upgrade() {
            set_server_pack(&ui, ServerPackTarget::NewServer, None);
        }
    });

    let weak = ui.as_weak();
    ui.on_clear_import_server_pack(move || {
        if let Some(ui) = weak.upgrade() {
            set_server_pack(&ui, ServerPackTarget::Import, None);
        }
    });
}

/// Shows a chosen archive, or clears the choice.
///
/// The full path is what the backend needs and the file name is what the dialog
/// shows: a path long enough to be interesting is long enough to push
/// everything else out of a modal.
fn set_server_pack(ui: &AppWindow, target: ServerPackTarget, path: Option<&Path>) {
    let (full, name) = path.map_or_else(
        || (SharedString::default(), SharedString::default()),
        |path| {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            (path.display().to_string().into(), name.into())
        },
    );
    match target {
        ServerPackTarget::NewServer => {
            ui.set_create_server_pack(full);
            ui.set_create_server_pack_name(name);
        }
        ServerPackTarget::Import => {
            ui.set_import_server_pack(full);
            ui.set_import_server_pack_name(name);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn register_callbacks(
    ui: &AppWindow,
    commands: &Sender<Command>,
    creations: &Rc<RefCell<CreationTracker>>,
    live_viewer: &Rc<RefCell<LiveViewerState>>,
    import_flow: &Rc<RefCell<ImportFlowState>>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    browser: &Rc<ModBrowser>,
    mod_lists: &Rc<RefCell<ModLists>>,
    local_mods: &Rc<RefCell<LocalModDrop>>,
) {
    register_catalog_actions(ui, commands);
    register_mod_filter(ui, mod_lists);
    register_client_actions(ui, commands, Rc::clone(creations));
    register_mod_actions(ui, commands, browser);
    register_create_instance_naming(ui, commands);
    register_import_flow(ui, commands, import_flow, creations);
    register_server_pack_choice(ui, commands);
    register_create_server(ui, commands, Rc::clone(creations));

    let sender = commands.clone();
    ui.on_select_server(move |_| send(&sender, Command::Refresh));

    register_server_actions(ui, commands, live_viewer);
    register_mod_state_actions(ui, commands, local_mods);
    register_diagnostics(ui, commands);
    register_live_viewer(ui, commands, live_viewer);
    register_settings(ui, commands);
    register_minecraft_defaults(ui, commands, minecraft);

    let weak = ui.as_weak();
    ui.on_copy_text(move |text| {
        if let Some(ui) = weak.upgrade() {
            copy_to_clipboard(&ui, text.into());
        }
    });

    let sender = commands.clone();
    ui.on_cancel_operation(move |id| send(&sender, Command::CancelOperation(id.into())));
}

thread_local! {
    /// The clipboard this window owns, kept for as long as the window is.
    ///
    /// On X11 a selection has an owning window, and the owner is the context
    /// itself: whoever wants to paste asks that window for the bytes. A context
    /// built per copy is therefore destroyed before anyone can ask, and the
    /// clipboard is left empty -- the copy does not merely fail, it takes
    /// whatever was there with it. So one context lives as long as the thread
    /// that owns the window, and every copy goes through it.
    static CLIPBOARD: RefCell<Option<ClipboardContext>> = const { RefCell::new(None) };
}

fn copy_to_clipboard(ui: &AppWindow, text: String) {
    let copied = CLIPBOARD.with(|clipboard| {
        let mut clipboard = clipboard.borrow_mut();
        if clipboard.is_none() {
            *clipboard = ClipboardContext::new().ok();
        }
        clipboard
            .as_mut()
            .is_some_and(|clipboard| clipboard.set_contents(text).is_ok())
    });
    ui.invoke_present_toast(
        if copied {
            lookup("gui-clipboard-copied")
        } else {
            lookup("gui-clipboard-unavailable")
        }
        .into(),
        !copied,
    );
}

fn register_catalog_actions(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    ui.on_refresh(move || send(&sender, Command::Refresh));

    let sender = commands.clone();
    ui.on_request_version_catalog(move || send(&sender, Command::LoadMinecraftReleases));

    let sender = commands.clone();
    ui.on_warm_language_catalog(move |minecraft| {
        if !minecraft.is_empty() {
            send(&sender, Command::WarmLanguageCatalog(minecraft.into()));
        }
    });

    let sender = commands.clone();
    ui.on_request_loader_catalog(move |form, loader, minecraft| {
        if let Some(loader) = loader_kind_of(&loader) {
            send(
                &sender,
                Command::LoadLoaderVersions {
                    form: form.into(),
                    loader,
                    minecraft: minecraft.into(),
                },
            );
        }
    });
}

/// Wires the "New Instance" modal's own name-suggestion mechanics: opening it,
/// recomputing the suggestion, and noticing when the user takes it over.
fn register_create_instance_naming(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_open_create_instance_modal(move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        // The field starts empty, so the suggestion beside it is what an
        // untouched dialog means.
        ui.set_create_name(SharedString::default());
        ui.set_modal_error(SharedString::default());
        send(&sender, Command::LoadMinecraftReleases);
        if !ui.get_create_minecraft().is_empty() {
            send(
                &sender,
                Command::WarmLanguageCatalog(ui.get_create_minecraft().into()),
            );
        }
        ui.set_modal("create-instance".into());
        recompute_create_name(&ui);
    });

    let weak = ui.as_weak();
    ui.on_recompute_create_name(move || {
        if let Some(ui) = weak.upgrade() {
            recompute_create_name(&ui);
        }
    });

    let weak = ui.as_weak();
    ui.on_recompute_create_server_name(move || {
        if let Some(ui) = weak.upgrade() {
            recompute_create_server_name(&ui);
        }
    });

    register_loader_options(ui);
}

/// A drop of jars part-way through being answered for.
///
/// Held here rather than in the window because the answers are folded as they
/// are given: "overwrite all" is not a state the markup has to carry, it is the
/// rest of the list decided at once.
#[derive(Debug, Default)]
struct LocalModDrop {
    host: Option<ModHost>,
    /// Files nothing was asked about, plus every clash answered "overwrite".
    accepted: Vec<PathBuf>,
    /// The clashes still to answer for, in the order they were dropped.
    clashing: Vec<backend::LocalModClashData>,
    /// How far through `clashing` the questions have got.
    index: usize,
    /// How many the user waved away, so the result can say so.
    skipped: usize,
}

impl LocalModDrop {
    /// Starts over from one plan, discarding anything half-answered.
    fn begin(&mut self, host: ModHost, plan: &backend::LocalModPlanData) {
        self.host = Some(host);
        self.accepted.clone_from(&plan.ready);
        self.clashing.clone_from(&plan.clashing);
        self.index = 0;
        self.skipped = 0;
    }

    /// Records one answer and says whether anything is still to ask.
    fn answer(&mut self, answer: &str) -> bool {
        match answer {
            "overwrite" => {
                if let Some(clash) = self.clashing.get(self.index) {
                    self.accepted.push(clash.source.clone());
                }
                self.index += 1;
            }
            "skip" => {
                self.skipped += 1;
                self.index += 1;
            }
            "overwrite-all" => {
                self.accepted.extend(
                    self.clashing[self.index..]
                        .iter()
                        .map(|it| it.source.clone()),
                );
                self.index = self.clashing.len();
            }
            // "skip-all", and anything unrecognized, which must not silently
            // overwrite a file.
            _ => {
                self.skipped += self.clashing.len() - self.index;
                self.index = self.clashing.len();
            }
        }
        self.index < self.clashing.len()
    }
}

/// Every mod each list has, before the query narrows it.
///
/// The lists are kept here rather than in the window because filtering happens
/// in Rust: only the matching rows become `ModRow`s, so typing into the box
/// shrinks the number of cards the markup builds instead of hiding cards it
/// already built.
#[derive(Debug, Default)]
struct ModLists {
    client: Vec<ModRow>,
    server: Vec<ModRow>,
    /// Which host each list is currently for, so a query typed against one
    /// instance is not left filtering the next one's mods.
    client_host: String,
    server_host: String,
}

impl ModLists {
    fn rows(&self, host_kind: &str) -> &[ModRow] {
        if host_kind == "server" {
            &self.server
        } else {
            &self.client
        }
    }

    /// Replaces one list, answering whether it is now about a different host.
    fn replace(&mut self, host_kind: &str, host: &str, rows: Vec<ModRow>) -> bool {
        let (current, target) = if host_kind == "server" {
            self.server = rows;
            (&mut self.server_host, host)
        } else {
            self.client = rows;
            (&mut self.client_host, host)
        };
        let changed = current != target;
        if changed {
            target.clone_into(current);
        }
        changed
    }
}

/// The rows whose name, file name, or provider contains `query`.
///
/// Case-insensitive and substring rather than prefix, because a user looking
/// for a mod types the part of the name they remember and it is rarely the
/// start. An empty query matches everything, so clearing the box is what
/// restores the list.
fn matching_mods(rows: &[ModRow], query: &str) -> Vec<ModRow> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return rows.to_vec();
    }
    rows.iter()
        .filter(|row| {
            [&row.title, &row.file_name, &row.provider, &row.version]
                .into_iter()
                .any(|field| field.to_lowercase().contains(&needle))
        })
        .cloned()
        .collect()
}

/// Redraws one mod list through its current query.
fn render_mod_list(ui: &AppWindow, lists: &ModLists, host_kind: &str) {
    let query = if host_kind == "server" {
        ui.get_server_mods_query()
    } else {
        ui.get_managed_mods_query()
    };
    let rows = matching_mods(lists.rows(host_kind), query.as_str());
    if host_kind == "server" {
        ui.set_server_mods(reconciled(&ui.get_server_mods(), rows));
    } else {
        ui.set_managed_mods(reconciled(&ui.get_managed_mods(), rows));
    }
}

/// A handle for one additional launch, unique for the life of this process.
///
/// It reaches a lock file name and a cancellation key, so it stays to
/// characters both can hold. The process id keeps two launchers from colliding
/// on one instance; the counter keeps this one from colliding with itself.
fn next_launch_handle() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Launching an instance, as the default account or as a chosen one.
fn register_launch_actions(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    ui.on_launch_client(move |id| {
        if let Some(instance) = backend::parse_instance_id(&id) {
            send(
                &sender,
                Command::LaunchClient {
                    instance,
                    account: None,
                    additional: None,
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_launch_client_as(move |id, account, alongside| {
        let Some(instance) = backend::parse_instance_id(&id) else {
            return;
        };
        // An empty choice is the default account, which is what plain Launch
        // does. Passing it through as a reference would ask the services to
        // resolve the empty string.
        let account = (!account.is_empty()).then(|| account.to_string());
        // A second client needs a handle of its own, so it can be listed and
        // stopped without touching the launch it is running beside.
        let additional = alongside.then(next_launch_handle);
        send(
            &sender,
            Command::LaunchClient {
                instance,
                account,
                additional,
            },
        );
    });
}

fn register_client_actions(
    ui: &AppWindow,
    commands: &Sender<Command>,
    creations: Rc<RefCell<CreationTracker>>,
) {
    register_promotion_actions(ui, commands);
    register_pack_link_actions(ui, commands);
    let sender = commands.clone();
    let weak = ui.as_weak();
    let submit_creations = Rc::clone(&creations);
    ui.on_submit_create_instance(move |name, minecraft, loader, loader_version, persistent| {
        let key = CreationKey::Client {
            name: name.trim().to_owned(),
            minecraft: minecraft.as_str().to_owned(),
            loader: loader.as_str().to_owned(),
            loader_version: loader_version.as_str().to_owned(),
            persistent,
        };
        let pending = pending_creation(&key, false);
        let Some(request_id) = submit_creations.borrow_mut().accept(key, pending) else {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_present_toast(lookup("gui-creation-already-running").into(), true);
            }
            return;
        };
        if let Some(ui) = weak.upgrade() {
            ui.set_modal(SharedString::default());
            ui.set_modal_error(SharedString::default());
            apply_pending_creations(&ui, &submit_creations.borrow());
        }
        send(
            &sender,
            Command::CreateClient {
                request_id,
                name: name.into(),
                minecraft: minecraft.into(),
                loader: loader.into(),
                loader_version: loader_version.into(),
                persistent,
            },
        );
    });

    register_instance_selection(ui, commands, creations);

    register_launch_actions(ui, commands);

    let sender = commands.clone();
    ui.on_kill_instance(move |id, launch| {
        if let Some(instance) = backend::parse_instance_id(&id) {
            send(
                &sender,
                Command::KillInstance {
                    instance,
                    launch: (!launch.is_empty()).then(|| launch.to_string()),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_launch_and_connect(move |client, server| {
        if let (Some(client), Some(server)) = (
            backend::parse_instance_id(&client),
            backend::parse_server_id(&server),
        ) {
            send(&sender, Command::LaunchAndConnect { client, server });
        }
    });

    let sender = commands.clone();
    ui.on_open_client_folder(move |id| {
        if let Some(id) = backend::parse_instance_id(&id) {
            send(&sender, Command::OpenClientFolder(id));
        }
    });

    let sender = commands.clone();
    ui.on_duplicate_client(move |id| {
        if let Some(id) = backend::parse_instance_id(&id) {
            // Asks first whether the copy should carry the mods this instance
            // records but no longer has. Nothing to ask is the common case, and
            // the answer comes back as an empty list that copies straight away.
            send(&sender, Command::PlanDuplicate(id));
        }
    });

    let sender = commands.clone();
    ui.on_delete_client(move |id, files| {
        if let Some(id) = backend::parse_instance_id(&id) {
            send(&sender, Command::DeleteClient { id, files });
        }
    });

    let sender = commands.clone();
    ui.on_delete_server(move |id, files| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::DeleteServer { id, files });
        }
    });

    let sender = commands.clone();
    let handle = ui.as_weak();
    ui.on_load_instance_workspace(move |id| {
        let Some(ui) = handle.upgrade() else { return };
        // Opening an instance is where the preferred catalog applies: a
        // preference decides what the browser starts on, and never moves the
        // catalog out from under a search the user is already reading. An
        // unresolvable recorded name leaves the browser where it is rather
        // than searching one catalog while the setting names another; the
        // settings screen is where that name is reported.
        let preferred = ui.get_default_mod_catalog();
        if let Some(catalog) = presentation::catalog_to_open_on(&preferred, &ui.get_mod_provider())
        {
            show_mod_catalog(&ui, catalog);
        }
        if let Some(id) = backend::parse_instance_id(&id) {
            send(&sender, Command::LoadInstanceWorkspace(id));
        }
    });
}

fn register_mod_actions(ui: &AppWindow, commands: &Sender<Command>, browser: &Rc<ModBrowser>) {
    register_mod_mutations(ui, commands);
    register_mod_browser(ui, commands, browser);
    register_browser_install(ui, commands, browser);
    register_favorite_versions(ui, commands);

    let weak = ui.as_weak();
    ui.on_open_mod_project_page(move |url| {
        if url.is_empty() {
            return;
        }
        let opened = ephervex_core::open_in_browser(url.as_str());
        if let Some(ui) = weak.upgrade() {
            ui.invoke_present_toast(
                lookup(if opened {
                    "gui-mod-project-page-opened"
                } else {
                    "gui-mod-project-page-open-failed"
                })
                .into(),
                !opened,
            );
        }
    });

    let sender = commands.clone();
    let handle = ui.as_weak();
    ui.on_open_mod_manager(move |path| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(instance) = backend::parse_instance_id(&ui.get_selected_instance_id()) else {
            return;
        };
        // The modal opens on the state the list already has, then fills in.
        // Waiting for the catalog before showing anything would make an offline
        // instance feel broken.
        ui.set_mod_action_path(path.clone());
        ui.set_mod_action_name(mod_row_title(&ui, &path));
        ui.set_mod_action_notice(SharedString::default());
        ui.set_mod_action_selected(SharedString::default());
        ui.set_mod_action_project_url(SharedString::default());
        ui.set_mod_versions(model(Vec::new()));
        ui.set_mod_action_can_change(false);
        ui.set_mod_action_can_update(false);
        ui.set_mod_action_can_remove(false);
        ui.set_mod_action_up_to_date(false);
        ui.set_mod_action_pending(false);
        ui.set_mod_action_loading(true);
        ui.set_modal_error(SharedString::default());
        ui.set_modal("mod-manage".into());
        send(
            &sender,
            Command::LoadModVersions {
                instance,
                path: PathBuf::from(path.as_str()),
            },
        );
    });
}

/// Searching a catalog, paging further into the results, and switching which
/// catalog is being searched.
///
/// Deciding what fits this instance and resolving a dependency graph both
/// happen in `ephervex-core`; this side never asks a provider anything.
fn register_mod_browser(ui: &AppWindow, commands: &Sender<Command>, browser: &Rc<ModBrowser>) {
    // One counter for every search request this browser ever sends, searches,
    // "load more", and the continuations of a page size larger than one
    // request alike, so two requests that end up identical in every other way
    // (the same instance, catalog, text, and offset, asked for twice because
    // the user switched catalog and back) still mint distinct ids. Owned by
    // the browser rather than read from the query/provider/offset the request
    // carries: those are exactly the fields that can repeat.
    let sender = commands.clone();
    let handle = ui.as_weak();
    let ids = Rc::clone(browser);
    ui.on_search_mods(move |instance, provider, query| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(instance) = backend::parse_instance_id(&instance) else {
            return;
        };
        let Some(catalog) = ModProvider::parse(&provider) else {
            return;
        };
        // Minted before anything about the browser's own state changes: a
        // request id that could not be allocated must leave the browser
        // exactly as it was, not part-way into a request that was never
        // actually sent.
        let Some(request_id) = next_mod_search_request_id(&ids.search_requests) else {
            return;
        };
        // What was submitted, which is not what the fields say: the user may
        // keep typing, or switch catalog, while this one answers, and only the
        // answer to what was actually asked belongs on screen.
        ui.set_mod_search_submitted(query.clone());
        ui.set_mod_search_provider(provider);
        ui.set_mod_search_ran(true);
        ui.set_mod_search_loading(true);
        ui.set_mod_search_notice(SharedString::default());
        // A new search replaces the list rather than adding to it, so the rows
        // on screen always belong to one question.
        ui.set_mod_results(model(Vec::new()));
        ui.set_mod_results_total(0);
        ui.set_mod_results_next_offset(-1);
        // A fresh search always starts at zero; a page arriving for any other
        // offset belongs to a "load more" this search has superseded.
        ui.set_mod_search_requested_offset(0);
        // What this fill is aiming for. A page size larger than one request
        // reaches it by asking again, so the target is what says when to stop.
        ui.set_mod_results_target(ui.get_mod_page_size());
        // Instance, catalog, text, and offset can all repeat (typing the same
        // query again, or switching catalog away and back), and this is what
        // still tells this request apart from an older, unrelated one that
        // happens to look identical.
        ui.set_mod_search_expected_request_id(request_id.to_string().into());
        send(
            &sender,
            Command::SearchMods {
                instance,
                provider: catalog,
                query: query.to_string(),
                offset: 0,
                limit: request_limit(),
                request_id,
            },
        );
    });

    let sender = commands.clone();
    let handle = ui.as_weak();
    let ids = Rc::clone(browser);
    ui.on_load_more_mods(move |instance, provider, query, offset| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(instance) = backend::parse_instance_id(&instance) else {
            return;
        };
        let Some(catalog) = ModProvider::parse(&provider) else {
            return;
        };
        // Minted before anything about the browser's own state changes, for
        // the same reason a fresh search does this first: an id that could
        // not be allocated must leave the browser exactly as it was.
        let Some(request_id) = next_mod_search_request_id(&ids.search_requests) else {
            return;
        };
        // Continues the search already on screen, at the offset the provider
        // itself named as the continuation. Nothing is fetched ahead of what
        // was asked for, and an answer for any other offset is stale.
        ui.set_mod_search_loading(true);
        ui.set_mod_search_requested_offset(offset);
        // One more page on top of what is already shown, rather than a target
        // the list has long since passed.
        ui.set_mod_results_target(
            to_i32(ui.get_mod_results().row_count()) + ui.get_mod_page_size(),
        );
        // Its own fresh id, from the same counter a fresh search uses: a
        // "load more" is just as much an individual request as a search is,
        // and its offset alone does not distinguish it from another one
        // fired at the same offset for a superseded question.
        ui.set_mod_search_expected_request_id(request_id.to_string().into());
        send(
            &sender,
            Command::SearchMods {
                instance,
                provider: catalog,
                query: query.to_string(),
                offset: offset.try_into().unwrap_or(0),
                limit: request_limit(),
                request_id,
            },
        );
    });

    let handle = ui.as_weak();
    ui.on_switch_mod_catalog(move |provider| {
        let Some(ui) = handle.upgrade() else { return };
        show_mod_catalog(&ui, &provider);
    });

    // Held for the session only. It is a reading preference for one panel, not
    // a setting the launcher has any reason to remember.
    let handle = ui.as_weak();
    ui.on_set_mod_page_size(move |label| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(size) = presentation::result_page_size(&label) else {
            return;
        };
        ui.set_mod_page_size(to_i32(size));
        ui.set_mod_page_size_label(label);
    });
}

/// Points the browser at one catalog and retires everything the previous one
/// could still be answered by.
///
/// Switching asks a different question, and everything that could let a search
/// still in flight for the catalog just left behind land on this one has to
/// move together, in this one step, or a late answer can still slip past the
/// staleness guard. Shared with opening an instance so that arriving at the
/// browser on the preferred catalog invalidates exactly as much as clicking the
/// chip for it does, rather than through a second set of rules.
fn show_mod_catalog(ui: &AppWindow, provider: &str) {
    let reset = presentation::CatalogSwitchReset::for_provider(provider);
    ui.set_mod_provider(provider.into());
    ui.set_mod_search_provider(reset.provider.into());
    // Retired, not just momentarily mismatched: switching back to this
    // catalog before an old answer returns must not reinstate the exact
    // question it was waiting on. The expected request id is retired the
    // same way: `submitted` alone cannot survive a later search for the
    // exact same words, which fills it back in with the exact same value.
    ui.set_mod_search_submitted(reset.submitted.into());
    ui.set_mod_search_expected_request_id(reset.request_id.into());
    ui.set_mod_search_loading(reset.loading);
    ui.set_mod_search_requested_offset(reset.requested_offset);
    // A new catalog replaces the list rather than showing the old one's
    // rows under a different heading.
    ui.set_mod_results(model(Vec::new()));
    ui.set_mod_search_ran(false);
    ui.set_mod_search_notice(SharedString::default());
    ui.set_mod_results_total(0);
    ui.set_mod_results_next_offset(-1);
}

/// Narrows the loader choice to the ones a Minecraft release ever had.
///
/// `form` is `"client"` or `"server"`, matching the dialog the choice belongs
/// to. A loader that drops out of the list cannot stay selected, so the choice
/// falls back to Vanilla along with the build list that belonged to it.
fn apply_loader_options(ui: &AppWindow, form: &str, minecraft: &str) {
    let server = form == "server";
    let current = if server {
        ui.get_create_server_loader()
    } else {
        ui.get_create_loader()
    };
    let choices = presentation::loader_choices(minecraft, &current);
    let options = model(
        choices
            .options
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    );
    if server {
        ui.set_create_server_loaders(options);
        if choices.reset {
            ui.set_create_server_loader(choices.selected.into());
            ui.set_create_server_loader_version(SharedString::default());
            ui.set_create_server_loader_versions(model(Vec::new()));
        }
    } else {
        ui.set_create_loaders(options);
        if choices.reset {
            ui.set_create_loader(choices.selected.into());
            ui.set_create_loader_version(SharedString::default());
            ui.set_create_loader_versions(model(Vec::new()));
        }
    }
}

/// Pinning and unpinning a Minecraft release.
///
/// Both callbacks send the whole list rather than one change, because the order
/// is the setting: a favorite added is appended, and one removed leaves the
/// order of the rest alone. The list on screen is not touched here; the saved
/// value comes back in the next snapshot, so what is shown is always what is
/// stored.
fn register_favorite_versions(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_toggle_favorite_version(move |version| {
        let Some(ui) = weak.upgrade() else { return };
        let favorites = presentation::toggled_favorite(&favorite_versions(&ui), &version);
        send(&sender, Command::SetFavoriteMinecraftVersions(favorites));
    });

    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_remove_favorite_version(move |version| {
        let Some(ui) = weak.upgrade() else { return };
        let mut favorites = favorite_versions(&ui);
        favorites.retain(|kept| kept.as_str() != version.as_str());
        send(&sender, Command::SetFavoriteMinecraftVersions(favorites));
    });
}

/// A typed path, or `None` where the field was left to the global setting.
fn optional_path(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// A typed number, or `None` where the field was left to the global setting.
///
/// Something that is not a number is also `None`: the field is numeric, and
/// refusing to save the rest of the form over one stray character would be
/// worse than the value it could not read staying inherited.
fn optional_number(value: &str) -> Option<u32> {
    value.trim().parse().ok()
}

fn favorite_versions(ui: &AppWindow) -> Vec<String> {
    ui.get_favorite_minecraft_versions()
        .iter()
        .map(|version| version.to_string())
        .collect()
}

fn register_loader_options(ui: &AppWindow) {
    let weak = ui.as_weak();
    ui.on_minecraft_selection_changed(move |form, minecraft| {
        if let Some(ui) = weak.upgrade() {
            apply_minecraft_selection(&ui, &form, &minecraft);
        }
    });
}

/// What follows from choosing a Minecraft release in a creation dialog: which
/// loaders it can have, whether its star is on, and what the instance is
/// called.
///
/// The suggested name is recomputed here rather than only where the user picked
/// something, because a release that does not offer the selected loader resets
/// it. A name worked out before that reset describes a loader the dialog no
/// longer holds, and creating from it produced an instance whose name and
/// target disagreed.
fn apply_minecraft_selection(ui: &AppWindow, form: &str, minecraft: &str) {
    apply_loader_options(ui, form, minecraft);
    if form == "server" {
        recompute_create_server_name(ui);
        return;
    }
    ui.set_create_minecraft_favorite(favorite_versions(ui).iter().any(|kept| kept == minecraft));
    recompute_create_name(ui);
}

/// Installing a mod straight from the browser.
///
/// Installing is planning first: core resolves the newest release that fits the
/// instance and everything it requires, and only then is anything written. What
/// this adds is that a plan with nothing left to decide is applied without a
/// confirmation, because the button already said what it does. A plan that does
/// carry a decision, which today means linked servers to choose between, still
/// opens the confirmation; see `apply_mod_install_plan`.
///
/// Kept apart from `register_mod_browser`: this is not a search, carries no
/// request id or offset of its own, and does not touch any of the search
/// staleness bookkeeping.
fn register_browser_install(ui: &AppWindow, commands: &Sender<Command>, browser: &Rc<ModBrowser>) {
    let sender = commands.clone();
    let handle = ui.as_weak();
    let install_queue = Rc::clone(browser);
    ui.on_install_from_browser(move |instance, provider, project| {
        let Some(ui) = handle.upgrade() else { return };
        if backend::parse_instance_id(&instance).is_none()
            || ModProvider::parse(&provider).is_none()
        {
            return;
        }
        let request = BrowserInstall {
            instance: instance.to_string(),
            provider: provider.to_string(),
            project: project.to_string(),
        };
        let start = install_queue.installs.borrow_mut().enqueue(request);
        if let Some(request) = start {
            start_browser_install(&ui, &sender, &request);
        }
        mark_queued_results(&ui, &install_queue.installs.borrow());
    });

    let handle = ui.as_weak();
    let sender = commands.clone();
    let install_queue = Rc::clone(browser);
    ui.on_abandon_browser_install(move || {
        let Some(ui) = handle.upgrade() else { return };
        let next = install_queue.installs.borrow_mut().finish_active();
        advance_browser_install(&ui, &sender, &install_queue, next);
    });
}

/// Asks for one queued install's plan and records the question it answers.
fn start_browser_install(ui: &AppWindow, commands: &Sender<Command>, request: &BrowserInstall) {
    let (Some(instance), Some(catalog)) = (
        backend::parse_instance_id(&request.instance),
        ModProvider::parse(&request.provider),
    ) else {
        return;
    };
    ui.set_mod_plan_requested(request.project.as_str().into());
    ui.set_mod_plan_requested_provider(request.provider.as_str().into());
    ui.set_mod_plan_direct(true);
    // The rows an earlier confirmation left behind name servers chosen for a
    // different operation, and a plan that needs no confirmation submits
    // whatever is in this model.
    ui.set_linked_targets(model(Vec::new()));
    send(
        commands,
        Command::PlanMod {
            instance,
            provider: catalog,
            project: request.project.clone(),
        },
    );
}

/// Starts whatever the queue handed back, and repaints the rows either way.
fn advance_browser_install(
    ui: &AppWindow,
    commands: &Sender<Command>,
    browser: &Rc<ModBrowser>,
    next: Option<BrowserInstall>,
) {
    if let Some(next) = next {
        start_browser_install(ui, commands, &next);
    }
    mark_queued_results(ui, &browser.installs.borrow());
}

/// The next opaque id for a mod search request, unique for the life of this
/// process, or `None` once the counter is exhausted.
///
/// A plain counter rather than a timestamp or a random value: nothing about
/// ordering or unpredictability is wanted here, only that two requests are
/// never told apart by anything a caller could also produce by asking the
/// same question twice. Checked rather than saturating: a counter that
/// saturated would hand the same id, `u64::MAX`, to every request from that
/// point on, which is exactly the collision this exists to prevent. The
/// counter is advanced only when an id was actually handed out, so a caller
/// that keeps asking after exhaustion keeps getting `None` rather than a
/// repeated one. At realistic request rates this ceiling is not reachable
/// within any process's lifetime; the caller still has to treat it as a real
/// outcome rather than assume it away.
fn next_mod_search_request_id(counter: &Cell<u64>) -> Option<u64> {
    let next = counter.get().checked_add(1)?;
    counter.set(next);
    Some(next)
}

/// The actions that change an instance's mods, kept apart from the ones that
/// only look at them.
fn register_mod_mutations(ui: &AppWindow, commands: &Sender<Command>) {
    register_loader_change(ui, commands);

    let sender = commands.clone();
    let handle = ui.as_weak();
    ui.on_change_mod_version(move |instance, path, version| {
        let Some(ui) = handle.upgrade() else { return };
        if let Some(instance) = backend::parse_instance_id(&instance) {
            send(
                &sender,
                Command::ChangeModVersion {
                    instance,
                    path: PathBuf::from(path.as_str()),
                    version: version.to_string(),
                    servers: chosen_linked_servers(&ui),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_plan_mod_apply(move |instance, path, version| {
        if let Some(instance) = backend::parse_instance_id(&instance) {
            send(
                &sender,
                Command::PlanModApply {
                    instance,
                    path: PathBuf::from(path.as_str()),
                    version: version.to_string(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_plan_mod_removal(move |instance, path| {
        if let Some(instance) = backend::parse_instance_id(&instance) {
            send(
                &sender,
                Command::PlanModRemoval {
                    instance,
                    path: PathBuf::from(path.as_str()),
                },
            );
        }
    });

    let sender = commands.clone();
    let handle = ui.as_weak();
    ui.on_remove_mod(move |instance, path| {
        let Some(ui) = handle.upgrade() else { return };
        if let Some(instance) = backend::parse_instance_id(&instance) {
            send(
                &sender,
                Command::RemoveMod {
                    instance,
                    path: PathBuf::from(path.as_str()),
                    servers: chosen_linked_servers(&ui),
                },
            );
        }
    });

    let sender = commands.clone();
    let handle = ui.as_weak();
    ui.on_install_mod(move |instance, provider, project, version| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(catalog) = ModProvider::parse(&provider) else {
            return;
        };
        if let Some(instance) = backend::parse_instance_id(&instance) {
            send(
                &sender,
                Command::InstallMod {
                    instance,
                    provider: catalog,
                    project: project.to_string(),
                    // The exact release the confirmation showed, from the
                    // catalog the confirmation was for.
                    version: version.to_string(),
                    servers: chosen_linked_servers(&ui),
                },
            );
        }
    });
}

/// The linked servers the user ticked, by their own stable ids.
///
/// Read out of the rows rather than tracked separately, so what is submitted is
/// exactly what the confirmation showed. A row whose id no longer parses is
/// dropped rather than guessed at: acting on a position instead would let a
/// deleted server hand its selection to whichever one took its place.
fn chosen_linked_servers(ui: &AppWindow) -> Vec<ephervex_model::ServerId> {
    ui.get_linked_targets()
        .iter()
        .filter(|row| row.actionable && row.selected)
        .filter_map(|row| backend::parse_server_id(&row.id))
        .collect()
}

/// Moving the open instance to a different build of its own loader.
fn register_loader_change(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    let handle = ui.as_weak();
    ui.on_open_loader_change(move || {
        let Some(ui) = handle.upgrade() else { return };
        let Ok(index) = usize::try_from(ui.get_selected_instance_index()) else {
            return;
        };
        let Some(row) = ui.get_instances().row_data(index) else {
            return;
        };
        // Vanilla has no build to change; the control that opens this is
        // hidden for it, and this is the second gate.
        let Some(loader) = loader_kind_of(&row.loader_kind) else {
            return;
        };
        ui.set_loader_change_current(row.loader.clone());
        ui.set_loader_change_selected(SharedString::default());
        ui.set_loader_change_versions(model(Vec::new()));
        ui.set_loader_change_loading(true);
        ui.set_loader_change_pending(false);
        ui.set_modal_error(SharedString::default());
        ui.set_modal("loader-change".into());
        send(
            &sender,
            Command::LoadLoaderVersions {
                form: "loader-change".to_owned(),
                loader,
                minecraft: row.minecraft.to_string(),
            },
        );
    });

    let sender = commands.clone();
    ui.on_change_loader_version(move |instance, version| {
        if let Some(instance) = backend::parse_instance_id(&instance) {
            send(
                &sender,
                Command::ChangeLoaderVersion {
                    instance,
                    version: version.to_string(),
                },
            );
        }
    });
}

fn register_instance_selection(
    ui: &AppWindow,
    commands: &Sender<Command>,
    creations: Rc<RefCell<CreationTracker>>,
) {
    let sender = commands.clone();
    let weak = ui.as_weak();
    // What this handler last acted on. The markup assigns `selected-instance-id`
    // before calling in, so the property cannot tell a new selection from a
    // repeat of the current one.
    let selected = Rc::new(RefCell::new(String::new()));
    ui.on_select_instance(move |id| {
        let repeat = *selected.borrow() == id.as_str();
        if !repeat {
            id.as_str().clone_into(&mut selected.borrow_mut());
        }
        if let Some(ui) = weak.upgrade() {
            let linked = ui
                .get_servers()
                .iter()
                .filter(|row| row.linked_client_id == id)
                .collect::<Vec<_>>();
            ui.set_linked_servers(reconciled(&ui.get_linked_servers(), linked));
            apply_pending_creations(&ui, &creations.borrow());
        }
        // Clicking the already-selected card changes nothing that a snapshot
        // would answer, and asking for one anyway is what used to rebuild the
        // list between the two halves of a double click.
        if !repeat {
            send(&sender, Command::Refresh);
        }
    });
}

/// Which mods a host has, and whether each one loads.
///
/// Its own registration rather than part of the server actions: the same two
/// callbacks serve a client instance's panel and a server's, and neither
/// belongs to one screen.
fn register_mod_state_actions(
    ui: &AppWindow,
    commands: &Sender<Command>,
    drop: &Rc<RefCell<LocalModDrop>>,
) {
    register_local_mod_actions(ui, commands, drop);

    let sender = commands.clone();
    ui.on_load_server_mods(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::LoadServerMods(id));
        }
    });

    let sender = commands.clone();
    ui.on_load_running(move || send(&sender, Command::LoadRunning));

    let sender = commands.clone();
    ui.on_force_stop_launch(move |id, launch| {
        if let Some(instance) = backend::parse_instance_id(&id) {
            send(
                &sender,
                Command::ForceStopLaunch {
                    instance,
                    launch: (!launch.is_empty()).then(|| launch.to_string()),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_refresh_mods(move |kind, id| {
        if let Some(host) = backend::parse_mod_host(&kind, &id) {
            send(&sender, Command::RefreshMods(host));
        }
    });

    let sender = commands.clone();
    ui.on_disable_suggested_mods(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::DisableSuggestedMods(id));
        }
    });

    let sender = commands.clone();
    ui.on_restore_mod_file(move |kind, host, path| {
        let Some(host) = backend::parse_mod_host(&kind, &host) else {
            return;
        };
        send(
            &sender,
            Command::RestoreModFile {
                host,
                path: PathBuf::from(path.as_str()),
            },
        );
    });

    let sender = commands.clone();
    ui.on_set_mod_enabled(move |kind, host, path, enabled| {
        let Some(host) = backend::parse_mod_host(&kind, &host) else {
            return;
        };
        send(
            &sender,
            Command::SetModEnabled {
                host,
                path: PathBuf::from(path.as_str()),
                enabled,
            },
        );
    });
}

/// Opening a modpack a web page offered, and the sites agreed to serve one.
///
/// Whether the scheme is claimed at all is not here: that is settled by
/// installing (ADR 0028). What the frontend owns is the prompt a link raises and
/// the list of answers it has already been given.
fn register_pack_link_actions(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    ui.on_forget_pack_host(move |host| {
        send(&sender, Command::ForgetPackHost(host.to_string()));
    });

    // Read once at startup, so the sites already agreed to are on the page
    // before anyone opens it.
    send(commands, Command::LoadLinkSettings);

    let sender = commands.clone();
    ui.on_deliver_pack_link(move |url| {
        send(
            &sender,
            Command::OpenPackUrl {
                url: url.to_string(),
                consent: None,
            },
        );
    });

    let sender = commands.clone();
    ui.on_open_pack_link(move |url, remember| {
        let consent = if remember {
            ephervex_core::HostConsent::Remember
        } else {
            ephervex_core::HostConsent::JustOnce
        };
        send(
            &sender,
            Command::OpenPackUrl {
                url: url.to_string(),
                consent: Some(consent),
            },
        );
    });
}

/// Hands the window a modpack link, as a desktop scheme handler delivers one.
///
/// Takes the window rather than the controller because both callers have one:
/// `main` for the link this process was started for, and the single-instance
/// listener for a link a later launch handed over.
pub fn open_pack_link(ui: &AppWindow, url: &str) {
    ui.invoke_deliver_pack_link(url.into());
}

/// Brings the window forward, for a link that arrived at a launcher already
/// running.
///
/// Best effort by nature. Whether a window may raise itself is the display
/// server's decision and not every one allows it, so this asks and does not
/// check: a link that opens the import dialog behind another window is still
/// better than a second launcher.
pub fn raise(ui: &AppWindow) {
    let window = ui.window();
    window.set_minimized(false);
    window.show().unwrap_or_else(|error| {
        tracing::debug!(%error, "could not bring the window forward");
    });
}

fn register_promotion_actions(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    ui.on_open_make_persistent(move |id| {
        if let Some(id) = backend::parse_instance_id(&id) {
            send(&sender, Command::PlanPromotion(id));
        }
    });

    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_submit_make_persistent(move |id| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let Some(instance) = backend::parse_instance_id(&id) else {
            return;
        };
        let servers = ui
            .get_promote_servers()
            .iter()
            .filter(|server| server.selected)
            .filter_map(|server| backend::parse_server_id(&server.id))
            .collect();
        send(&sender, Command::PromoteInstance { instance, servers });
    });
}

/// Dropping jars onto a mod list, deleting one, and renaming what holds them.
fn register_local_mod_actions(
    ui: &AppWindow,
    commands: &Sender<Command>,
    drop: &Rc<RefCell<LocalModDrop>>,
) {
    let sender = commands.clone();
    let pending = Rc::clone(drop);
    let weak = ui.as_weak();
    ui.on_resolve_mod_clash(move |answer| {
        let Some(ui) = weak.upgrade() else { return };
        let mut pending = pending.borrow_mut();
        if pending.answer(answer.as_str()) {
            ui.set_mod_clash_index(i32::try_from(pending.index).unwrap_or(i32::MAX));
            return;
        }
        ui.set_modal(SharedString::default());
        submit_local_mods(&sender, &pending);
    });

    let sender = commands.clone();
    ui.on_pick_local_mods(move |kind, host| {
        if let Some(host) = backend::parse_mod_host(&kind, &host) {
            send(&sender, Command::PickLocalMods(host));
        }
    });

    let sender = commands.clone();
    ui.on_delete_hand_placed_mod(move |kind, host, path| {
        let Some(host) = backend::parse_mod_host(&kind, &host) else {
            return;
        };
        send(
            &sender,
            Command::DeleteHandPlacedMod {
                host,
                path: PathBuf::from(path.as_str()),
            },
        );
    });

    let sender = commands.clone();
    ui.on_rename_entity(move |kind, id, name| {
        let name = name.trim().to_owned();
        match kind.as_str() {
            "client" => {
                if let Some(id) = backend::parse_instance_id(&id) {
                    send(&sender, Command::RenameClient { id, name });
                }
            }
            "server" => {
                if let Some(id) = backend::parse_server_id(&id) {
                    send(&sender, Command::RenameServer { id, name });
                }
            }
            _ => {}
        }
    });

    let sender = commands.clone();
    ui.on_duplicate_client_deciding(move |id, drop_absent| {
        if let Some(instance) = backend::parse_instance_id(&id) {
            send(
                &sender,
                Command::DuplicateClient {
                    instance,
                    absent_mods: if drop_absent {
                        AbsentModPolicy::Drop
                    } else {
                        AbsentModPolicy::Keep
                    },
                },
            );
        }
    });
}

/// Sends whatever a finished drop ended up agreeing to.
///
/// A drop that agreed to nothing still reports itself, so waving away every
/// clash says so rather than looking like a gesture that did not register.
fn submit_local_mods(sender: &Sender<Command>, pending: &LocalModDrop) {
    let Some(host) = pending.host else { return };
    send(
        sender,
        Command::AddLocalMods {
            host,
            sources: pending.accepted.clone(),
            skipped: pending.skipped,
        },
    );
}

/// Selects one server and puts its console on screen, following its output.
///
/// The console is a tab of the server workspace, which is itself a branch of
/// the servers page. Start is offered from a client's workspace too, so the
/// page has to be set alongside the workspace: the body renders whichever
/// branch matches both, and a pair that matches none renders nothing at all.
/// Following is turned on because the output being waited for has not been
/// written yet.
fn show_server_console(ui: &AppWindow, server: SharedString) {
    // Which client, if any, Back returns to. Coming from a client's workspace
    // that is the client the server was started from. Coming from the servers
    // list there is none, and a value left over from an earlier visit would
    // send Back to an unrelated instance. Already inside the server workspace,
    // whatever brought the user there still holds.
    if ui.get_page() == INSTANCES_PAGE && ui.get_workspace() == "instance" {
        ui.set_server_workspace_return_client(ui.get_selected_instance_id());
    } else if ui.get_workspace() != "server" {
        ui.set_server_workspace_return_client(SharedString::default());
    }
    ui.set_selected_server_id(server.clone());
    ui.set_page(SERVERS_PAGE);
    ui.set_workspace("server".into());
    ui.set_workspace_tab("Console".into());
    ui.set_log_query(SharedString::default());
    ui.set_log_follow(true);
    // What resolves the row the whole workspace indexes and loads the crash
    // notice. Without it the header has no name and the command field stays
    // disabled until the next snapshot happens to arrive.
    ui.invoke_load_server_workspace(server);
}

/// Starting a server, which also puts its console on screen.
///
/// Start is offered from the server card, the workspace header, and the
/// overview, and every one of them would otherwise have to navigate for itself.
/// Done once here, so all of them land on the output the run is about to
/// produce.
fn register_server_start(
    ui: &AppWindow,
    commands: &Sender<Command>,
    live_viewer: &Rc<RefCell<LiveViewerState>>,
) {
    let sender = commands.clone();
    let weak = ui.as_weak();
    let start_state = Rc::clone(live_viewer);
    ui.on_server_start(move |id| {
        let Some(server) = backend::parse_server_id(&id) else {
            return;
        };
        send(&sender, Command::StartServer(server));

        let source = LiveLogSource::Server(server);
        start_state.borrow_mut().activate(source.clone());
        if let Some(ui) = weak.upgrade() {
            show_server_console(&ui, id);
            render_live_viewer(&ui, &mut start_state.borrow_mut());
        }
        send(&sender, Command::OpenLiveLog(source));
    });
}

fn register_server_actions(
    ui: &AppWindow,
    commands: &Sender<Command>,
    live_viewer: &Rc<RefCell<LiveViewerState>>,
) {
    let sender = commands.clone();
    ui.on_save_instance_runtime(move |id, java, min, max, args| {
        let Some(instance) = backend::parse_instance_id(&id) else {
            return;
        };
        send(
            &sender,
            Command::SaveInstanceRuntime {
                instance,
                java: optional_path(&java),
                min_memory_mib: optional_number(&min),
                max_memory_mib: optional_number(&max),
                // Split on whitespace: a JVM argument never contains one, and
                // one field is easier to reason about than a list editor for
                // something typed twice a year.
                jvm_args: args.split_whitespace().map(ToOwned::to_owned).collect(),
            },
        );
    });

    let sender = commands.clone();
    ui.on_clear_instance_runtime(move |id| {
        if let Some(instance) = backend::parse_instance_id(&id) {
            send(
                &sender,
                Command::SaveInstanceRuntime {
                    instance,
                    java: None,
                    min_memory_mib: None,
                    max_memory_mib: None,
                    jvm_args: Vec::new(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_repair_server(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::RepairServer(id));
        }
    });

    register_server_property_actions(ui, commands);

    let sender = commands.clone();
    ui.on_open_server_folder(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::OpenServerFolder(id));
        }
    });

    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_load_server_workspace(move |id| {
        if let Some(ui) = weak.upgrade() {
            let index = ui.get_servers().iter().position(|row| row.id == id);
            ui.set_selected_server_id(id.clone());
            ui.set_selected_server_index(index.map_or(-1, to_i32));
        }
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::LoadServerWorkspace(id));
        }
    });

    register_server_start(ui, commands, live_viewer);

    let sender = commands.clone();
    ui.on_server_stop(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::StopServer(id));
        }
    });

    let sender = commands.clone();
    ui.on_server_restart(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::RestartServer(id));
        }
    });

    let sender = commands.clone();
    ui.on_server_kill(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::KillServer(id));
        }
    });

    let sender = commands.clone();
    ui.on_server_console(move |id, command| {
        if let Some(server) = backend::parse_server_id(&id)
            && !command.trim().is_empty()
        {
            send(
                &sender,
                Command::SendConsole {
                    server,
                    command: command.into(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_accept_eula(move || send(&sender, Command::AcceptEula));
    let sender = commands.clone();
    ui.on_revoke_eula(move || send(&sender, Command::RevokeEula));
    let sender = commands.clone();
    ui.on_open_eula_reference(move || send(&sender, Command::OpenEulaReference));
}

fn register_server_property_actions(ui: &AppWindow, commands: &Sender<Command>) {
    let weak = ui.as_weak();
    ui.on_validate_server_properties(move |access, address, port, max_players| {
        if let Some(ui) = weak.upgrade() {
            let port = validate_port(&port);
            let address = validate_ip(&address, access == "Custom");
            let players = validate_player_count(&max_players);
            ui.set_property_port_error(port.message.into());
            ui.set_property_address_error(address.message.into());
            ui.set_property_max_players_error(players.into());
        }
    });

    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_save_server_properties(
        move |id,
              motd,
              max_players,
              difficulty,
              gamemode,
              allow_flight,
              pvp,
              allowlist,
              online_mode,
              access,
              address,
              port| {
            let Some(server) = backend::parse_server_id(&id) else {
                return;
            };
            let port_result = validate_port(&port);
            let address_result = validate_ip(&address, access == "Custom");
            let max_players_error = validate_player_count(&max_players);
            if let Some(ui) = weak.upgrade() {
                ui.set_property_port_error(port_result.message.clone().into());
                ui.set_property_address_error(address_result.message.clone().into());
                ui.set_property_max_players_error(max_players_error.clone().into());
            }
            let max_players_valid = max_players_error.is_empty();
            let difficulty_valid =
                ["peaceful", "easy", "normal", "hard"].contains(&difficulty.as_str());
            let gamemode_valid =
                ["survival", "creative", "adventure", "spectator"].contains(&gamemode.as_str());
            let Ok(network) = parse_network(&access, &address, online_mode, &port) else {
                return;
            };
            if !max_players_valid || !difficulty_valid || !gamemode_valid {
                if let Some(ui) = weak.upgrade() {
                    ui.invoke_present_toast(
                        lookup("gui-server-property-values-invalid").into(),
                        true,
                    );
                }
                return;
            }
            send(
                &sender,
                Command::SaveServerProperties {
                    server,
                    network,
                    values: vec![
                        ("motd".to_owned(), motd.into()),
                        ("max-players".to_owned(), max_players.into()),
                        ("difficulty".to_owned(), difficulty.into()),
                        ("gamemode".to_owned(), gamemode.into()),
                        ("allow-flight".to_owned(), allow_flight.to_string()),
                        ("pvp".to_owned(), pvp.to_string()),
                        ("white-list".to_owned(), allowlist.to_string()),
                    ],
                },
            );
        },
    );
}

fn validate_player_count(value: &str) -> String {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return lookup("gui-digits-only");
    }
    if value.parse::<u32>().is_ok_and(|players| players > 0) {
        String::new()
    } else {
        lookup("gui-player-count-minimum")
    }
}

fn register_diagnostics(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    ui.on_load_diagnostics(move |scope, id, kind| {
        if let (Some(scope), Some(kind)) = (
            backend::parse_scope(&scope, &id),
            backend::parse_diagnostic_kind(&kind),
        ) {
            send(&sender, Command::LoadDiagnostics { scope, kind });
        }
    });

    let sender = commands.clone();
    ui.on_open_diagnostic(move |scope, id, kind, name| {
        if let (Some(scope), Some(kind)) = (
            backend::parse_scope(&scope, &id),
            backend::parse_diagnostic_kind(&kind),
        ) {
            send(
                &sender,
                Command::OpenDiagnostic {
                    scope,
                    kind,
                    name: name.into(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_refresh_diagnostic(move |scope, id, kind, name| {
        if let (Some(scope), Some(kind)) = (
            backend::parse_scope(&scope, &id),
            backend::parse_diagnostic_kind(&kind),
        ) {
            send(
                &sender,
                Command::OpenDiagnostic {
                    scope,
                    kind,
                    name: name.into(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_search_diagnostic(move |scope, id, kind, name, query| {
        if let (Some(scope), Some(kind)) = (
            backend::parse_scope(&scope, &id),
            backend::parse_diagnostic_kind(&kind),
        ) {
            send(
                &sender,
                Command::SearchDiagnostic {
                    scope,
                    kind,
                    name: name.into(),
                    query: query.into(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_open_diagnostic_folder(move |scope, id, kind| {
        if let (Some(scope), Some(kind)) = (
            backend::parse_scope(&scope, &id),
            backend::parse_diagnostic_kind(&kind),
        ) {
            send(&sender, Command::OpenDiagnosticFolder { scope, kind });
        }
    });
}

fn register_settings(ui: &AppWindow, commands: &Sender<Command>) {
    register_display_settings(ui, commands);

    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_save_linked_server_settings(move |show, make_operator, level| {
        let level = u8::try_from(level)
            .ok()
            .and_then(ephervex_model::OperatorPermissionLevel::from_value);
        if let Some(operator_level) = level {
            send(
                &sender,
                Command::SaveLinkedServerSettings {
                    show_globally: show,
                    make_operator,
                    operator_level,
                },
            );
        } else if let Some(ui) = weak.upgrade() {
            ui.invoke_present_toast(lookup("gui-operator-level-invalid").into(), true);
        }
    });

    let sender = commands.clone();
    ui.on_add_offline_account(move |name| {
        if !name.trim().is_empty() {
            send(&sender, Command::AddOfflineAccount(name.into()));
        }
    });

    let sender = commands.clone();
    ui.on_set_default_account(move |id| send(&sender, Command::SetDefaultAccount(id.into())));
    let sender = commands.clone();
    ui.on_remove_account(move |id| send(&sender, Command::RemoveAccount(id.into())));
    let sender = commands.clone();
    ui.on_begin_microsoft_login(move || send(&sender, Command::MicrosoftLogin));

    register_provider_identifiers(ui, commands);
    register_curseforge_credential(ui, commands);

    let sender = commands.clone();
    ui.on_save_storage_settings(move |root| {
        send(&sender, Command::SaveStorageRoot(root.into()));
    });
    register_artifact_cache(ui, commands);
    let sender = commands.clone();
    ui.on_choose_storage_root(move || send(&sender, Command::PickStorageRoot));
    let sender = commands.clone();
    ui.on_reset_storage_root(move || send(&sender, Command::SaveStorageRoot(String::new())));
    register_watched_folders(ui, commands);

    let sender = commands.clone();
    ui.on_refresh_java(move || send(&sender, Command::RefreshJava));

    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_save_java_settings(
        move |auto_detect, auto_download, minimum, maximum, preferred_path| {
            let (minimum_mib, maximum_mib) = match parse_java_memory(&minimum, &maximum) {
                Ok(memory) => memory,
                Err(message) => {
                    if let Some(ui) = weak.upgrade() {
                        ui.invoke_present_toast(message.into(), true);
                    }
                    return;
                }
            };
            send(
                &sender,
                Command::SaveJavaSettings {
                    auto_detect,
                    auto_download,
                    minimum_mib,
                    maximum_mib,
                    preferred_path: preferred_path.into(),
                },
            );
        },
    );
}

/// Wires the "watched download folders" list: adding one via a native
/// folder picker, and removing one. Each change sends the whole updated
/// list, matching how the settings screen's other list-shaped values save.
fn register_watched_folders(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    ui.on_choose_watched_folder(move || send(&sender, Command::PickWatchedFolder));

    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_remove_watched_folder(move |folder| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let remaining: Vec<String> = ui
            .get_watched_download_folders()
            .iter()
            .filter(|existing| existing != folder.as_str())
            .map(|existing| existing.to_string())
            .collect();
        send(&sender, Command::SaveWatchedDownloadFolders(remaining));
    });
}

/// The Minecraft defaults screen: one draft, one atomic save.
///
/// Every callback here edits the draft and nothing else. There is deliberately
/// no path from this screen that writes an instance's `options.txt`: the only
/// mutation it can produce is [`Command::SaveMinecraftPreferences`], which
/// changes what the *next* instance is created with.
#[allow(clippy::too_many_lines)]
fn register_minecraft_defaults(
    ui: &AppWindow,
    commands: &Sender<Command>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
) {
    ui.set_minecraft_categories(model(
        minecraft_defaults::CATEGORIES
            .iter()
            .map(|category| SharedString::from(minecraft_defaults::category_label(*category)))
            .collect(),
    ));
    ui.set_minecraft_aspect_labels(model(
        minecraft_defaults::WINDOW_ASPECTS
            .iter()
            .map(|aspect| SharedString::from(aspect.label()))
            .collect(),
    ));

    // -- what is shown. None of these touch the draft.
    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    ui.on_minecraft_select_category(move |index| {
        let Some(ui) = handle.upgrade() else { return };
        if let Ok(index) = usize::try_from(index) {
            state.borrow_mut().select_category(index);
        }
        ui.set_minecraft_capturing_id(SharedString::default());
        render_minecraft(&ui, &state);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    ui.on_minecraft_set_query(move |query| {
        let Some(ui) = handle.upgrade() else { return };
        state.borrow_mut().set_query(&query);
        ui.set_minecraft_capturing_id(SharedString::default());
        render_minecraft(&ui, &state);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    ui.on_minecraft_set_show_advanced(move |show| {
        let Some(ui) = handle.upgrade() else { return };
        state.borrow_mut().set_show_advanced(show);
        ui.set_minecraft_capturing_id(SharedString::default());
        render_minecraft(&ui, &state);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    ui.on_minecraft_set_only_overridden(move |only| {
        let Some(ui) = handle.upgrade() else { return };
        state.borrow_mut().set_only_overridden(only);
        ui.set_minecraft_capturing_id(SharedString::default());
        render_minecraft(&ui, &state);
    });

    // -- the compatibility preview, which changes what rows say and never
    //    what they hold.
    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_set_preview_release(move |release| {
        let Some(ui) = handle.upgrade() else { return };
        // Selecting the release already selected asks nothing new, so no
        // request is minted for it.
        if !state.borrow_mut().set_preview_release(&release) {
            return;
        }
        render_minecraft(&ui, &state);
        request_minecraft_preview(&ui, &state, &sender);
        request_minecraft_languages(&ui, &state, &sender, true);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_retry_languages(move || {
        let Some(ui) = handle.upgrade() else { return };
        request_minecraft_languages(&ui, &state, &sender, true);
        render_minecraft(&ui, &state);
    });

    // -- the draft itself.
    register_minecraft_editors(ui, commands, minecraft);
    register_minecraft_capture(ui, commands, minecraft);

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_save_defaults(move || {
        let Some(ui) = handle.upgrade() else { return };
        // Marked in flight the instant the batch is taken and released only by
        // a completion, so one click cannot become two batches.
        let Some(edits) = state.borrow_mut().begin_save() else {
            return;
        };
        refresh_minecraft_totals(&ui, &state);
        send(&sender, Command::SaveMinecraftPreferences { edits });
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_discard_defaults(move || {
        let Some(ui) = handle.upgrade() else { return };
        state.borrow_mut().discard();
        ui.set_minecraft_capturing_id(SharedString::default());
        render_minecraft(&ui, &state);
        request_minecraft_preview(&ui, &state, &sender);
    });
}

/// The value editors, each of which changes exactly one preference's draft.
fn register_minecraft_editors(
    ui: &AppWindow,
    commands: &Sender<Command>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
) {
    let picker = WindowSizePicker::default();
    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_set_boolean(move |id, on| {
        edit_minecraft_draft(&handle, &state, &sender, &id, |model, id| {
            model.set_boolean(id, on);
        });
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_set_number(move |id, number| {
        edit_minecraft_draft(&handle, &state, &sender, &id, |model, id| {
            model.set_number(id, f64::from(number));
        });
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_set_enumeration(move |id, index| {
        edit_minecraft_draft(&handle, &state, &sender, &id, |model, id| {
            if let Ok(index) = usize::try_from(index) {
                model.set_enumeration(id, index);
            }
        });
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_set_language(move |id, index| {
        edit_minecraft_draft(&handle, &state, &sender, &id, |model, id| {
            if let Ok(index) = usize::try_from(index) {
                model.set_language(id, index);
            }
        });
    });

    // Typing is recorded without rebuilding the rows: rebuilding them would
    // recreate the field the user is in and take the focus with it. Only the
    // figures that decide whether Save is offered are refreshed live.
    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    ui.on_minecraft_edit_number(move |id, text| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(id) = minecraft_preference_id(&id) else {
            return;
        };
        state.borrow_mut().set_number_text(&id, &text);
        refresh_minecraft_totals(&ui, &state);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    ui.on_minecraft_edit_window(move |id, width, height| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(id) = minecraft_preference_id(&id) else {
            return;
        };
        state.borrow_mut().set_window_size(&id, &width, &height);
        refresh_minecraft_totals(&ui, &state);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_commit_number(move |id, text| {
        edit_minecraft_draft(&handle, &state, &sender, &id, |model, id| {
            model.set_number_text(id, &text);
        });
    });

    register_minecraft_window_picker(ui, commands, minecraft, &picker);
    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_commit_text(move |id, text| {
        edit_minecraft_draft(&handle, &state, &sender, &id, |model, id| {
            model.set_text(id, &text);
        });
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    let following = picker.clone();
    ui.on_minecraft_forget_preference(move |id| {
        edit_minecraft_draft(
            &handle,
            &state,
            &sender,
            &id,
            MinecraftDefaultsModel::reset_row,
        );
        follow_window_size(&following, &state, &id);
    });
}

/// The sizing window, and the size this build last put it on.
///
/// Cheap to clone: every holder shares the one window and the one token.
#[derive(Default, Clone)]
struct WindowSizePicker {
    window: Rc<RefCell<Option<WindowSizePreview>>>,
    /// The row the window is currently standing for, when it is up.
    showing: Rc<RefCell<Option<ephervex_core::PreferenceId>>>,
    /// The size the window itself last reported.
    ///
    /// The window's own history, which is what says which edge is being
    /// dragged. While a ratio is held this deliberately differs from the size
    /// on record, so the value cannot stand in for it.
    reported: Rc<Cell<Option<WindowSize>>>,
    /// The edge this drag is driving, decided once and held until it settles.
    driving: Rc<Cell<Option<WindowAxis>>>,
    /// Fires once the window has stopped moving.
    ///
    /// A window manager resizes a window from the pointer for as long as the
    /// user holds it, and resizing it from here at the same time is two
    /// parties dragging one window. So a held ratio moves the numbers as the
    /// user drags and moves the window itself once, after the drag is over.
    settle: Rc<Timer>,
    /// The size this build put the window on, held until the window reports a
    /// different one.
    ///
    /// A window reports the size it was given exactly as it reports a size the
    /// user dragged it to, and the two mean opposite things: opening a window
    /// to look at a size must store nothing, while dragging it is the choice
    /// itself. It also reports its width and its height as two separate
    /// changes, so one size arrives more than once. While the window is still
    /// on the size this build put it on, nothing has moved and nothing is
    /// stored; the first size that is not that one clears this, and every
    /// report after it is the user's.
    applied: Rc<Cell<Option<WindowSize>>>,
}

/// How long the window has to hold still before it is taken to be at rest.
const WINDOW_SETTLE: Duration = Duration::from_millis(250);

/// How long to wait before saying the aspect again after a size is applied.
///
/// Long enough for the toolkit to have republished the window's size limits,
/// which it does after a size change and which drops the one field it does not
/// model.
const ASPECT_REPUBLISH: Duration = Duration::from_millis(80);

/// The ratio the picker is holding the window to, if any.
fn held_ratio(minecraft: &Rc<RefCell<MinecraftDefaultsModel>>) -> Option<(u32, u32)> {
    let model = minecraft.borrow();
    model
        .window_aspect()
        .ratio()
        .filter(|_| model.window_locked())
}

/// Says on the window's face what size it is, and asks its manager to keep it
/// on the chosen shape.
///
/// The ratio goes out as an aspect the manager holds the window to, so every
/// edge and corner keeps working and the manager moves whichever one the user
/// is not holding. Re-sent on each report because the toolkit rewrites the
/// same property whenever the window's own size limits change.
fn describe_window(
    window: &WindowSizePreview,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    size: WindowSize,
) {
    let held = held_ratio(minecraft);
    window.set_readout(window_size_readout(size).into());
    window.set_holds_ratio(held.is_some());
    window_aspect::hold(window.window(), held);
}

/// Says the aspect again once the toolkit has finished republishing the
/// window's size limits over it.
fn republish_aspect(window: &WindowSizePreview, ratio: Option<(u32, u32)>) {
    let again = window.as_weak();
    Timer::single_shot(ASPECT_REPUBLISH, move || {
        if let Some(window) = again.upgrade() {
            window_aspect::hold(window.window(), ratio);
        }
    });
}

/// Puts the window on one exact size.
fn put_window_on(window: &WindowSizePreview, applied: &Cell<Option<WindowSize>>, size: WindowSize) {
    applied.set(Some(size));
    window
        .window()
        .set_size(LogicalSize::new(to_edge(size.width), to_edge(size.height)));
}

impl WindowSizePicker {
    /// Puts the window on one exact size and says so on its face.
    fn put_on(
        &self,
        window: &WindowSizePreview,
        minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
        size: WindowSize,
    ) {
        describe_window(window, minecraft, size);
        put_window_on(window, &self.applied, size);
        // Applying a size is what makes the toolkit republish the limits, so
        // this is where the aspect has to be said twice.
        republish_aspect(window, held_ratio(minecraft));
    }

    /// Follows one row's size while the window is standing for that row.
    ///
    /// The window is what the setting looks like, so a size the row takes from
    /// anywhere - a ratio, a typed pair, a reset - moves the window with it. A
    /// window left showing a size the setting no longer holds is the one thing
    /// a picker made of a real window exists to rule out.
    fn follow(
        &self,
        minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
        id: &ephervex_core::PreferenceId,
    ) {
        if self.showing.borrow().as_ref() != Some(id) {
            return;
        }
        let slot = self.window.borrow();
        let Some(window) = slot.as_ref() else { return };
        if !window.window().is_visible() {
            return;
        }
        let size = minecraft.borrow().window_start(id);
        self.put_on(window, minecraft, size);
    }

    /// Takes the window off screen, keeping it for the next time.
    fn hide(&self) {
        if let Some(window) = self.window.borrow().as_ref() {
            let _ = window.hide();
        }
        self.showing.borrow_mut().take();
        self.reported.set(None);
        self.driving.set(None);
    }
}

/// The window size picker: the two exact fields, the ratio it projects with,
/// and the window that is the size they describe.
fn register_minecraft_window_picker(
    ui: &AppWindow,
    commands: &Sender<Command>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    picker: &WindowSizePicker,
) {
    // Which field was edited is which edge a held ratio keeps, so the two
    // fields commit through their own axis rather than a shared guess.
    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    let following = picker.clone();
    ui.on_minecraft_commit_window_width(move |id, width, height| {
        edit_minecraft_row(&handle, &state, &sender, &id, |model, id| {
            model.commit_window_size(id, &width, &height, WindowAxis::Width);
        });
        follow_window_size(&following, &state, &id);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    let following = picker.clone();
    ui.on_minecraft_commit_window_height(move |id, width, height| {
        edit_minecraft_row(&handle, &state, &sender, &id, |model, id| {
            model.commit_window_size(id, &width, &height, WindowAxis::Height);
        });
        follow_window_size(&following, &state, &id);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    let following = picker.clone();
    ui.on_minecraft_set_window_aspect(move |id, index| {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let Some(aspect) = WindowAspect::at(index) else {
            return;
        };
        edit_minecraft_row(&handle, &state, &sender, &id, |model, id| {
            model.set_window_aspect(id, aspect);
        });
        follow_window_size(&following, &state, &id);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    let following = picker.clone();
    ui.on_minecraft_set_window_lock(move |id, held| {
        edit_minecraft_row(&handle, &state, &sender, &id, |model, id| {
            model.set_window_locked(id, held);
        });
        follow_window_size(&following, &state, &id);
    });

    // The picker is a real window. Opening it looks at the row and writes
    // nothing; only a resize is a choice, and each one settles the row on its
    // own so the rest of the screen is left alone.
    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    let opening = picker.clone();
    ui.on_minecraft_open_window_preview(move |id| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(id) = minecraft_preference_id(&id) else {
            return;
        };
        let start = state.borrow().window_start(&id);
        let mut slot = opening.window.borrow_mut();
        if slot.is_none() {
            let Ok(window) = WindowSizePreview::new() else {
                return;
            };
            window.set_minimum_edge(minimum_window_edge());
            register_i18n(&window);
            *slot = Some(window);
        }
        let Some(window) = slot.as_ref() else { return };
        // Rewired on every open, so the window always reports to the row that
        // opened it.
        register_window_size_preview(window, &ui, &state, &sender, &id, &opening);
        opening.showing.borrow_mut().replace(id);
        opening.reported.set(Some(start));
        opening.driving.set(None);
        opening.put_on(window, &state, start);
        let _ = window.show();
    });

    // Closing the application takes the sizing window with it: Slint runs the
    // loop until the last window closes, so one left open would keep the
    // application alive with no way back to it.
    let closing = picker.clone();
    ui.window().on_close_requested(move || {
        closing.hide();
        slint::CloseRequestResponse::HideWindow
    });
}

/// Closes out a resize once the window has stopped moving.
///
/// Restarted by every report, so it runs at the end of a run of them rather
/// than during it: the previewed release is asked about the new value once,
/// where a question per pointer movement would be a question per frame. The
/// edge the gesture was driving is released here too, so the next drag decides
/// its own.
///
/// It deliberately does not resize the window. A window manager owns the
/// window for as long as the user holds it, and there is no way from here to
/// tell "held still" from "let go": resizing it on a guess means this build
/// and the pointer both dragging one window, which climbs away from the user
/// as each correction gives the manager a larger frame to re-derive from.
/// Sizes this build chooses are pushed only at moments it starts itself.
fn settle_window_size(
    picker: &WindowSizePicker,
    ui: &slint::Weak<AppWindow>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    commands: &Sender<Command>,
) {
    let handle = ui.clone();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    // Only the pieces the settling needs, so the timer holds nothing that
    // holds the timer.
    let driving = Rc::clone(&picker.driving);
    picker
        .settle
        .start(TimerMode::SingleShot, WINDOW_SETTLE, move || {
            driving.set(None);
            let Some(ui) = handle.upgrade() else { return };
            request_minecraft_preview(&ui, &state, &sender);
        });
}

/// Moves the sizing window onto whatever one row now holds.
fn follow_window_size(
    picker: &WindowSizePicker,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    id: &SharedString,
) {
    if let Some(id) = minecraft_preference_id(id) {
        picker.follow(minecraft, &id);
    }
}

/// The exact size one window stands for, as its own title says it.
fn window_size_readout(size: WindowSize) -> String {
    lookup_args(
        "gui-minecraft-window-preview-readout",
        [
            ("width", Arg::from(size.width.to_string().as_str())),
            ("height", Arg::from(size.height.to_string().as_str())),
        ],
    )
}

/// One window edge as the toolkit measures it.
#[allow(clippy::cast_precision_loss)]
fn to_edge(edge: u32) -> f32 {
    edge as f32
}

/// One window edge as this build stores it, or `None` for a figure no window
/// on screen could report.
///
/// The ceiling is the largest edge the display server itself can express, so
/// this refuses a nonsensical figure rather than imposing a limit on what may
/// be configured: the fields still take any size Minecraft would.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn from_edge(edge: f32) -> Option<u32> {
    let rounded = edge.round();
    (rounded.is_finite() && rounded >= 0.0 && rounded <= f32::from(u16::MAX))
        .then_some(rounded as u32)
}

/// The smallest window this build accepts, in the toolkit's own units.
fn minimum_window_edge() -> f32 {
    to_edge(WindowSize::MINIMUM_EDGE)
}

/// Wires one sizing window to the row it is sizing.
///
/// The window is the value: its size goes to the model as-is, and the exact
/// size the model settles on comes back to the window when the two differ, so
/// a held ratio is visible on screen rather than only in the fields. That
/// correction reports the size it just applied, which the model then leaves
/// alone, so the two agree after one step instead of chasing each other.
fn register_window_size_preview(
    window: &WindowSizePreview,
    ui: &AppWindow,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    commands: &Sender<Command>,
    id: &ephervex_core::PreferenceId,
    picker: &WindowSizePicker,
) {
    let ui_handle = ui.as_weak();
    let ui_settling = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    let preference = id.clone();
    let resized = window.as_weak();
    let gesture = picker.clone();
    window.on_resized(move |width, height| {
        let Some(ui) = ui_handle.upgrade() else {
            return;
        };
        let (Some(width), Some(height)) = (from_edge(width), from_edge(height)) else {
            return;
        };
        let reported = WindowSize { width, height };
        let previous = gesture.reported.replace(Some(reported));
        // The window arriving on the size this build just gave it is not the
        // user moving anything.
        if gesture.applied.get() == Some(reported) {
            return;
        }
        gesture.applied.set(None);
        // One edge for the whole drag: decided by the first report that moves
        // the window, and kept until the window is at rest again.
        let anchor = gesture
            .driving
            .get()
            .or_else(|| previous.and_then(|previous| window_anchor(previous, reported)))
            .unwrap_or(WindowAxis::Width);
        gesture.driving.set(Some(anchor));

        let exact = state.borrow_mut().fit_window(&preference, reported, anchor);
        if let Some(window) = resized.upgrade() {
            // Also republishes the height a held ratio allows, so the manager
            // clamps the next movement of this same drag to it.
            describe_window(&window, &state, exact.unwrap_or(reported));
        }
        refresh_minecraft_row(&ui, &state, &preference);
        settle_window_size(&gesture, &ui_settling, &state, &sender);
    });

    let dismissed = picker.clone();
    window.on_dismissed(move || dismissed.hide());
}

/// Entering, leaving, and completing key capture.
///
/// Beginning and cancelling deliberately do not rebuild the rows: the control
/// that just took focus lives inside the row, and rebuilding would destroy it
/// mid-capture. That is why the capturing row is named by a property beside
/// the model rather than by a field inside it.
fn register_minecraft_capture(
    ui: &AppWindow,
    commands: &Sender<Command>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
) {
    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    ui.on_minecraft_begin_capture(move |id| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(parsed) = minecraft_preference_id(&id) else {
            return;
        };
        state.borrow_mut().begin_capture(&parsed);
        ui.set_minecraft_capturing_id(id);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    ui.on_minecraft_cancel_capture(move || {
        let Some(ui) = handle.upgrade() else { return };
        state.borrow_mut().cancel_capture();
        ui.set_minecraft_capturing_id(SharedString::default());
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_captured_key(move |id, reported| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(parsed) = minecraft_preference_id(&id) else {
            return;
        };
        // The toolkit reports a character. Which key that identifies, and
        // whether it identifies one at all, is decided in Rust.
        let Some(character) = reported.chars().next() else {
            return;
        };
        let outcome = state.borrow_mut().capture_key(&parsed, character);
        apply_capture_outcome(&ui, &state, &sender, outcome);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_captured_mouse(move |id, reported| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(parsed) = minecraft_preference_id(&id) else {
            return;
        };
        let outcome = state.borrow_mut().capture_mouse(&parsed, &reported);
        apply_capture_outcome(&ui, &state, &sender, outcome);
    });
}

/// What the screen does once a capture has resolved.
///
/// A refused press leaves capture open and changes nothing, so the user can
/// simply press something else. Every other outcome ends capture.
fn apply_capture_outcome(
    ui: &AppWindow,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    commands: &Sender<Command>,
    outcome: minecraft_defaults::CaptureOutcome,
) {
    use minecraft_defaults::CaptureOutcome;

    if outcome == CaptureOutcome::Refused {
        ui.invoke_present_toast(lookup("gui-minecraft-capture-refused").into(), true);
        return;
    }
    ui.set_minecraft_capturing_id(SharedString::default());
    render_minecraft(ui, minecraft);
    if outcome != CaptureOutcome::Canceled {
        request_minecraft_preview(ui, minecraft, commands);
    }
}

/// Applies one draft edit and shows the result.
///
/// Identity is the stable product id the row carries, never its position, so
/// an edit lands on the preference it was made against however the list has
/// been filtered or sorted since.
fn edit_minecraft_draft(
    handle: &slint::Weak<AppWindow>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    commands: &Sender<Command>,
    id: &SharedString,
    edit: impl FnOnce(&mut MinecraftDefaultsModel, &ephervex_core::PreferenceId),
) {
    let Some(ui) = handle.upgrade() else { return };
    let Some(id) = minecraft_preference_id(id) else {
        return;
    };
    edit(&mut minecraft.borrow_mut(), &id);
    render_minecraft(&ui, minecraft);
    request_minecraft_preview(&ui, minecraft, commands);
}

/// One committed edit to a row whose own controls have to survive it.
///
/// The draft changes exactly as it does for any other edit, but only the
/// edited row is written back. Rebuilding the list would recreate the control
/// the user is still in, which is how tabbing from one window size field to
/// the other loses the field it just moved to.
fn edit_minecraft_row(
    handle: &slint::Weak<AppWindow>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    commands: &Sender<Command>,
    id: &SharedString,
    edit: impl FnOnce(&mut MinecraftDefaultsModel, &ephervex_core::PreferenceId),
) {
    let Some(ui) = handle.upgrade() else { return };
    let Some(id) = minecraft_preference_id(id) else {
        return;
    };
    edit(&mut minecraft.borrow_mut(), &id);
    refresh_minecraft_row(&ui, minecraft, &id);
    request_minecraft_preview(&ui, minecraft, commands);
}

/// Reads a row's stable product id, refusing anything that is not one.
fn minecraft_preference_id(id: &SharedString) -> Option<ephervex_core::PreferenceId> {
    ephervex_core::PreferenceId::parse(id.as_str()).ok()
}

/// Shows one scope's diagnostic files, with nothing selected yet.
///
/// A new listing clears the previous selection along with it: leaving a file's
/// content on screen under a different scope's list is how a reader ends up
/// reading the wrong crash report.
fn apply_diagnostic_listing(
    ui: &AppWindow,
    rows: Vec<DiagnosticData>,
    scope: &str,
    scope_id: &str,
    kind: &str,
) {
    ui.set_diagnostics(model(diagnostic_rows(rows)));
    ui.set_selected_diagnostic_scope(scope.into());
    ui.set_selected_diagnostic_id(scope_id.into());
    ui.set_selected_diagnostic_kind(kind.into());
    ui.set_selected_diagnostic_name(SharedString::default());
    ui.set_diagnostic_content(SharedString::default());
    ui.set_diagnostic_path(SharedString::default());
    ui.set_diagnostic_title(SharedString::default());
}

/// What the reusable artifact cache's own answers do to the storage page.
fn apply_artifact_cache_event(ui: &AppWindow, event: Event) {
    match event {
        Event::ArtifactCacheStatus(status) => apply_artifact_cache_status(ui, status),
        // No new figure to show, so the previous one stands. What must not
        // stand is the clearing state: the toast beside this event is the
        // whole answer, and the button has to be usable again.
        Event::ArtifactCacheClearFinished => ui.set_artifact_cache_clearing(false),
        // Every other event is read by `apply_event`, which routed the two
        // above here.
        _ => {}
    }
}

/// Renders a fresh view of everything core holds.
///
/// The Minecraft defaults screen asks its two background questions again from
/// here, because a refreshed baseline can change both what the draft is and
/// what a release would do with it.
fn apply_snapshot_event(
    ui: &AppWindow,
    snapshot: Snapshot,
    creations: &Rc<RefCell<CreationTracker>>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    commands: &Sender<Command>,
) {
    apply_snapshot(ui, snapshot, minecraft);
    apply_pending_creations(ui, &creations.borrow());
    request_minecraft_preview(ui, minecraft, commands);
    request_minecraft_languages(ui, minecraft, commands, false);
}

/// What the Minecraft defaults screen does with its own background answers.
///
/// Both metadata answers are checked against the request they belong to before
/// anything is rendered: a reply resolved for a release the user has since
/// moved off must never replace the one they are looking at.
fn apply_minecraft_event(
    ui: &AppWindow,
    event: Event,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    commands: &Sender<Command>,
) {
    match event {
        Event::MinecraftPreferencesSaveFinished { succeeded } => {
            // Either way the save is over, so the screen stops holding itself
            // disabled. That is the whole point of a completion event: a
            // refused batch must leave Save and Discard usable again, and a
            // generic error carries no such promise because any unrelated
            // background work can raise one.
            //
            // On failure nothing about the draft is touched: not the overlay,
            // not the baseline, and not the text a user is part-way through
            // typing. The reason has already arrived as its own error.
            minecraft.borrow_mut().finish_save(succeeded);
            render_minecraft(ui, minecraft);
        }
        Event::MinecraftPreferencePreview {
            generation,
            minecraft: release,
            preview,
        } => {
            let accepted = {
                let mut model = minecraft.borrow_mut();
                let accepted = model.accepts_preview_result(generation, &release);
                if accepted {
                    model.apply_preview(preview);
                }
                accepted
            };
            if accepted {
                render_minecraft(ui, minecraft);
            }
        }
        Event::MinecraftLanguages {
            generation,
            minecraft: release,
            languages,
        } => {
            let accepted = {
                let mut model = minecraft.borrow_mut();
                let accepted = model.accepts_language_result(generation, &release);
                if accepted {
                    model.apply_languages(match languages {
                        Some(languages) => LanguageCatalogState::Available(languages),
                        None => LanguageCatalogState::Unavailable,
                    });
                }
                accepted
            };
            if accepted {
                render_minecraft(ui, minecraft);
                // The language verdict is part of what a release can do with
                // the draft, so the preview is asked again now that it is
                // known.
                request_minecraft_preview(ui, minecraft, commands);
            }
        }
        // Every other event is read by `apply_event`, which is what routed
        // the three above here.
        _ => {}
    }
}

/// Rebuilds the visible rows and everything derived from the draft.
fn render_minecraft(ui: &AppWindow, minecraft: &Rc<RefCell<MinecraftDefaultsModel>>) {
    let state = minecraft.borrow();
    ui.set_minecraft_preference_rows(model(state.rows()));
    ui.set_minecraft_category_index(to_i32(state.category_index()));
    ui.set_minecraft_show_advanced(state.show_advanced());
    ui.set_minecraft_only_overridden(state.only_overridden());
    ui.set_minecraft_preview_release(state.preview_release().into());
    ui.set_minecraft_languages_available(matches!(
        state.languages(),
        LanguageCatalogState::Available(_)
    ));
    ui.set_minecraft_preserved_notice(
        preserved_notice("gui-minecraft-preserved-notice", state.unrecognized()).into(),
    );
    ui.set_minecraft_unusable_notice(
        preserved_notice("gui-minecraft-unusable-notice", state.unusable()).into(),
    );
    drop(state);
    refresh_minecraft_totals(ui, minecraft);
}

/// Refreshes one row in place, leaving every other row's element alone.
///
/// Replacing the whole list rebuilds the elements in it, which would take the
/// pointer's grip on a control mid-gesture with it. Only the row that changed
/// is written back.
fn refresh_minecraft_row(
    ui: &AppWindow,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    id: &ephervex_core::PreferenceId,
) {
    let state = minecraft.borrow();
    if let Some((index, row)) = state.row_at(id) {
        ui.get_minecraft_preference_rows().set_row_data(index, row);
    }
    drop(state);
    refresh_minecraft_totals(ui, minecraft);
}

/// Refreshes only what decides whether the draft can be saved.
///
/// Separate from [`render_minecraft`] so a keystroke in a numeric field can
/// keep Save honest without rebuilding the field being typed into.
fn refresh_minecraft_totals(ui: &AppWindow, minecraft: &Rc<RefCell<MinecraftDefaultsModel>>) {
    let state = minecraft.borrow();
    // Mirrored from the model rather than tracked beside it, so the screen
    // cannot stay disabled after the model considers the save finished.
    ui.set_minecraft_saving(state.is_saving());
    ui.set_minecraft_dirty(state.is_dirty());
    ui.set_minecraft_can_save(state.can_save());
    ui.set_minecraft_dirty_count(to_i32(state.dirty_count()));
    ui.set_minecraft_total_count(to_i32(state.entries().len()));
}

/// One low-emphasis sentence about configuration this build leaves alone, or
/// nothing at all when there is none.
fn preserved_notice(message: &str, count: usize) -> String {
    if count == 0 {
        return String::new();
    }
    lookup_args(message, [("count", Arg::from(count))])
}

/// Asks what the previewed release would do with the draft as it now stands.
fn request_minecraft_preview(
    ui: &AppWindow,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    commands: &Sender<Command>,
) {
    let mut state = minecraft.borrow_mut();
    if state.preview_release().is_empty() {
        return;
    }
    // An identity that could not be minted means no request is sent at all:
    // reusing one would let an older answer be mistaken for this one.
    let Some(generation) = state.next_preview_request() else {
        return;
    };
    let request = Command::PreviewMinecraftPreferences {
        generation,
        minecraft: state.preview_release().to_owned(),
        draft: state.draft_preferences(),
        language: state.language_availability(),
    };
    drop(state);
    let _ = ui;
    send(commands, request);
}

/// Asks for the previewed release's own language list.
///
/// Only the language row depends on the answer, and a failure to get one is
/// reported there rather than anywhere else. `force` re-asks for a release
/// whose list is already resolved, which is what the retry action needs.
fn request_minecraft_languages(
    ui: &AppWindow,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    commands: &Sender<Command>,
    force: bool,
) {
    let mut state = minecraft.borrow_mut();
    if state.preview_release().is_empty() {
        return;
    }
    if !force && matches!(state.languages(), LanguageCatalogState::Available(_)) {
        return;
    }
    // Minted before the row is told anything is loading, so a request that
    // cannot be identified leaves the language row exactly as it was rather
    // than waiting forever on an answer nobody asked for.
    let Some(generation) = state.next_language_request() else {
        return;
    };
    state.apply_languages(LanguageCatalogState::Loading);
    let request = Command::LoadMinecraftLanguages {
        generation,
        minecraft: state.preview_release().to_owned(),
    };
    drop(state);
    ui.set_minecraft_languages_available(false);
    send(commands, request);
}

fn register_display_settings(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_save_display_settings(move |text_scale, technical_text_size| {
        let settings = u16::try_from(text_scale)
            .ok()
            .and_then(ephervex_core::ApplicationTextScale::from_percent)
            .zip(
                u16::try_from(technical_text_size)
                    .ok()
                    .and_then(ephervex_core::TechnicalTextSize::from_pixels),
            );
        if let Some((text_scale, technical_text_size)) = settings {
            send(
                &sender,
                Command::SaveDisplaySettings {
                    text_scale,
                    technical_text_size,
                },
            );
        } else if let Some(ui) = weak.upgrade() {
            ui.invoke_present_toast(lookup("gui-display-settings-invalid").into(), true);
        }
    });
}

fn parse_java_memory(minimum: &str, maximum: &str) -> Result<(u32, u32), String> {
    let parsed = minimum
        .parse::<u32>()
        .ok()
        .zip(maximum.parse::<u32>().ok())
        .ok_or_else(|| lookup("gui-java-memory-digits-only"))?;
    if parsed.0 == 0 || parsed.1 == 0 || parsed.0 > parsed.1 {
        Err(lookup("gui-java-memory-range-invalid"))
    } else {
        Ok(parsed)
    }
}

/// Puts a dropped batch of jars in front of the user, or gets on with it.
///
/// Nothing to ask is the common case and stays a single gesture: the files go
/// straight in. A refusal is reported either way, because a jar that was
/// dropped and did not arrive has to say why rather than vanish.
fn apply_local_mod_plan(
    ui: &AppWindow,
    plan: &backend::LocalModPlanData,
    local_mods: &Rc<RefCell<LocalModDrop>>,
    commands: &Sender<Command>,
) {
    let Some(host) = backend::parse_mod_host(&plan.host_kind, &plan.host) else {
        return;
    };
    // The list the user is looking at now, not the one they dropped onto: a
    // plan that arrives after they have navigated away has nowhere to land.
    if !mods_drop_intent(ui).is_some_and(|intent| intent == DropIntent::Mods(host)) {
        return;
    }

    // Said once, here, rather than folded into the count the copy reports: a
    // file the user dragged and that did not arrive has to say which one it was
    // and why, and "one could not be added" does not.
    render_local_mod_refusals(ui, plan);

    let mut pending = local_mods.borrow_mut();
    pending.begin(host, plan);
    if plan.clashing.is_empty() {
        if !plan.ready.is_empty() {
            submit_local_mods(commands, &pending);
        }
        return;
    }

    ui.set_mod_clashes(
        plan.clashing
            .iter()
            .map(|clash| LocalModClashRow {
                source: clash.source.display().to_string().into(),
                file_name: clash.file_name.clone().into(),
                existing_is_disabled: clash.existing_is_disabled,
            })
            .collect::<Vec<_>>()
            .as_slice()
            .into(),
    );
    ui.set_mod_clash_index(0);
    ui.set_modal("mod-clash".into());
}

/// Says which dropped files were not jars, once, as one line.
fn render_local_mod_refusals(ui: &AppWindow, plan: &backend::LocalModPlanData) {
    if plan.refused.is_empty() {
        return;
    }
    let detail = plan
        .refused
        .iter()
        .map(|it| format!("{} - {}", it.file_name, it.reason))
        .collect::<Vec<_>>()
        .join("; ");
    ui.invoke_queue_toast(detail.into(), true);
}

/// Asks what a copy should do with the mods whose jars are gone, if any are.
fn apply_absent_mods(ui: &AppWindow, absent: &backend::AbsentModsData, commands: &Sender<Command>) {
    let Some(instance) = backend::parse_instance_id(&absent.instance) else {
        return;
    };
    if absent.names.is_empty() {
        send(
            commands,
            Command::DuplicateClient {
                instance,
                absent_mods: AbsentModPolicy::Keep,
            },
        );
        return;
    }
    ui.set_absent_mods(
        absent
            .names
            .iter()
            .map(|name| SharedString::from(name.as_str()))
            .collect::<Vec<_>>()
            .as_slice()
            .into(),
    );
    ui.set_pending_action_id(absent.instance.as_str().into());
    ui.set_modal("absent-mods".into());
}

/// Opens the dialog that asks what to bring along.
///
/// Every offered server starts selected: a persistent instance whose server
/// disappears on the next reboot is the surprise this dialog exists to prevent,
/// so the safe answer is the default and unticking one is deliberate.
fn apply_promotion_plan(ui: &AppWindow, plan: &backend::PromotionPlanData) {
    ui.set_promote_instance_name(plan.name.as_str().into());
    ui.set_promote_destination(plan.destination.as_str().into());
    ui.set_promote_servers(ModelRc::new(VecModel::from(
        plan.servers
            .iter()
            .map(|(id, name)| PromoteServerRow {
                id: id.as_str().into(),
                display: name.as_str().into(),
                selected: true,
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_pending_action_id(plan.instance.as_str().into());
    ui.set_modal_error(SharedString::default());
    ui.set_modal("make-persistent".into());
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn apply_event(
    ui: &AppWindow,
    event: Event,
    activities: &Rc<RefCell<Vec<ActivityData>>>,
    creations: &Rc<RefCell<CreationTracker>>,
    import_flow: &Rc<RefCell<ImportFlowState>>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    browser: &Rc<ModBrowser>,
    mod_lists: &Rc<RefCell<ModLists>>,
    local_mods: &Rc<RefCell<LocalModDrop>>,
    commands: &Sender<Command>,
) {
    match event {
        Event::LocalModPlan(plan) => apply_local_mod_plan(ui, &plan, local_mods, commands),
        Event::AbsentMods(absent) => apply_absent_mods(ui, &absent, commands),
        Event::PromotionPlan(plan) => apply_promotion_plan(ui, &plan),
        Event::PackUrlConsent { url, display } => {
            ui.set_pack_link_url(url.as_str().into());
            ui.set_pack_link_display(display.as_str().into());
            ui.set_pack_link_remember(true);
            ui.set_modal_error(SharedString::default());
            ui.set_modal("open-pack-link".into());
        }
        Event::PackDownloaded { archive } => begin_import(ui, import_flow, commands, archive),
        Event::LinkSettings(settings) => {
            ui.set_trusted_pack_hosts(
                settings
                    .trusted_hosts
                    .iter()
                    .map(|host| SharedString::from(host.as_str()))
                    .collect::<Vec<_>>()
                    .as_slice()
                    .into(),
            );
        }
        Event::Snapshot(snapshot) => {
            apply_snapshot_event(ui, *snapshot, creations, minecraft, commands);
        }
        Event::MinecraftPreferencesSaveFinished { .. }
        | Event::MinecraftPreferencePreview { .. }
        | Event::MinecraftLanguages { .. } => {
            apply_minecraft_event(ui, event, minecraft, commands);
        }
        Event::ArtifactCacheStatus(_) | Event::ArtifactCacheClearFinished => {
            apply_artifact_cache_event(ui, event);
        }
        Event::InstanceRuntime(runtime) => {
            // A late answer for an instance the user has already left is
            // discarded rather than typed into the fields of another one.
            if presentation::result_is_current(&runtime.instance, &ui.get_selected_instance_id()) {
                ui.set_instance_java_path(runtime.java.into());
                ui.set_instance_memory_min(runtime.min_memory.into());
                ui.set_instance_memory_max(runtime.max_memory.into());
                ui.set_instance_jvm_args(runtime.jvm_args.into());
                ui.set_instance_runtime_pinned(runtime.pinned);
                ui.set_instance_runtime_global(runtime.global.into());
            }
        }
        Event::CreationProgress {
            request_id,
            detail,
            progress,
            indeterminate,
        } => {
            creations
                .borrow_mut()
                .progressed(&request_id, detail, progress, indeterminate);
            apply_pending_creations(ui, &creations.borrow());
        }
        Event::MinecraftReleases(versions) => {
            apply_minecraft_releases(ui, versions);
        }
        Event::LoaderVersions {
            form,
            loader,
            minecraft,
            versions,
        } => apply_loader_versions(ui, &form, loader, &minecraft, &versions),
        Event::ManagedMods(data) => {
            // A late answer for a workspace the user has already navigated
            // away from is discarded rather than rendered under the wrong
            // name. The workspace it belongs to already showed its durable
            // state, and reopening it asks again. The kind is checked as well
            // as the id, so a server's list can never land in the panel of the
            // instance it is linked to.
            if data.host_kind == "server" {
                if presentation::result_is_current(&data.host, &ui.get_selected_server_id()) {
                    let mut lists = mod_lists.borrow_mut();
                    if lists.replace("server", &data.host, managed_mod_rows(data.mods)) {
                        ui.set_server_mods_query(SharedString::default());
                    }
                    render_mod_list(ui, &lists, "server");
                    ui.set_server_mods_notice(data.notice.into());
                }
            } else if presentation::result_is_current(&data.host, &ui.get_selected_instance_id()) {
                let mut lists = mod_lists.borrow_mut();
                if lists.replace("client", &data.host, managed_mod_rows(data.mods)) {
                    ui.set_managed_mods_query(SharedString::default());
                }
                render_mod_list(ui, &lists, "client");
                drop(lists);
                ui.set_managed_mods_notice(data.notice.into());
                // What the instance has just changed, and the browser beside
                // this list says whether each result is already one of them.
                mark_installed_results(ui);
            }
        }
        Event::CrashNotice(notice) => {
            let current = if notice.host_kind == "server" {
                ui.get_selected_server_id()
            } else {
                ui.get_selected_instance_id()
            };
            if presentation::result_is_current(&notice.host, &current) {
                ui.set_crash_report(notice.report.into());
                ui.set_crash_failure(notice.failure.into());
                ui.set_crash_client_code_on_server(notice.client_code_on_server);
                ui.set_crash_suggested(i32::try_from(notice.suggested).unwrap_or(i32::MAX));
            }
        }
        Event::Running(running) => {
            ui.set_running(ModelRc::new(VecModel::from(
                running
                    .into_iter()
                    .map(|process| RunningRow {
                        kind: process.kind.into(),
                        id: process.id.into(),
                        launch: process.launch.into(),
                        name: process.name.into(),
                        pid: process.pid.into(),
                        uptime: process.uptime.into(),
                        usage: process.usage.into(),
                        last_line: process.last_line.into(),
                        controllable: process.controllable,
                        can_stop: process.can_stop,
                    })
                    .collect::<Vec<_>>(),
            )));
        }
        Event::ModVersions(data) => apply_mod_versions(ui, data),
        Event::ModRemovalPlan(data) => apply_mod_removal_plan(ui, data),
        Event::ModResults(results) => apply_mod_results(ui, results, browser, commands),
        Event::ModPlan(plan) => apply_mod_install_plan(ui, plan, browser, commands),
        Event::ModInstallFinished {
            instance,
            provider,
            project,
        } => {
            let finished = BrowserInstall {
                instance,
                provider,
                project,
            };
            let next = browser.installs.borrow_mut().finish(&finished);
            advance_browser_install(ui, commands, browser, next);
        }
        // The question will never be answered, so whatever asked it stops
        // waiting. The reason itself already arrived as an error.
        Event::ModPlanFailed {
            instance,
            provider,
            requested,
        } => {
            let abandoned = BrowserInstall {
                instance,
                provider,
                project: requested,
            };
            let next = browser.installs.borrow_mut().finish(&abandoned);
            advance_browser_install(ui, commands, browser, next);
        }
        Event::LinkedApply(section) => apply_linked_confirmation(ui, section),
        Event::LinkedResult(result) => apply_linked_result(ui, result),
        Event::ServerWorkspace(workspace) => apply_server_workspace(ui, workspace),
        Event::LiveLog(_) => {}
        Event::Diagnostics {
            rows,
            scope,
            scope_id,
            kind,
        } => apply_diagnostic_listing(ui, rows, &scope, &scope_id, &kind),
        Event::DiagnosticContent(content) => apply_diagnostic_content(ui, content),
        Event::DiagnosticUnavailable(message) => {
            ui.set_diagnostic_title(lookup("gui-diagnostic-unavailable").into());
            ui.set_diagnostic_path(SharedString::default());
            ui.set_diagnostic_content(message.into());
            ui.set_status_message(lookup("gui-diagnostic-disappeared").into());
        }
        Event::JavaRuntimes(rows) => ui.set_java_runtimes(model(java_rows(rows))),
        Event::Activity(activity) => {
            upsert_activity(&mut activities.borrow_mut(), activity);
            apply_activities(ui, &activities.borrow());
        }
        Event::Toast { message, error } => {
            ui.invoke_present_toast(message.into(), error);
        }
        Event::ModalError(message) => {
            ui.set_modal_error(message.into());
            // A refused mutation leaves its buttons usable again. Without this
            // the modal would show the reason and offer no way to act on it.
            ui.set_mod_action_pending(false);
        }
        Event::CloseModal => {
            ui.set_modal(SharedString::default());
            ui.set_modal_error(SharedString::default());
            // The operation that disabled the modal's actions has answered.
            ui.set_mod_action_pending(false);
        }
        // A new instance is what the user was just looking at the dialog for,
        // so it is what they are looking at now. The snapshot that follows this
        // keeps the selection, because it keeps whatever was selected.
        Event::ClientCreated { instance } => {
            reveal_instance(ui, &instance);
        }
        Event::CreationFinished { request_id } => {
            creations.borrow_mut().finish(&request_id);
            apply_pending_creations(ui, &creations.borrow());
        }
        // A discarded result belongs to an attempt this flow no longer tracks
        // (superseded, the modal was closed and reopened, or an unrelated
        // submission finished): there is nothing that should touch whatever
        // modal is open now. See `apply_import_result`.
        Event::ImportPickCanceled { generation } => {
            let applied = import_flow.borrow_mut().pick_canceled(generation);
            apply_import_result(ui, import_flow, applied);
        }
        Event::ImportAnalyzed {
            generation,
            archive,
            result,
        } => {
            let applied = import_flow
                .borrow_mut()
                .analyzed(generation, archive, result);
            apply_import_result(ui, import_flow, applied);
        }
        Event::ImportResolved { generation, result } => {
            let applied = import_flow.borrow_mut().resolved(generation, result);
            apply_import_result(ui, import_flow, applied);
        }
        Event::BlockedFileMatched {
            generation,
            identity,
        } => {
            let applied = import_flow
                .borrow_mut()
                .blocked_file_matched(generation, &identity);
            apply_import_result(ui, import_flow, applied);
        }
        Event::ImportSubmissionFinished {
            generation,
            succeeded,
            request_id,
        } => {
            if let Some(request_id) = request_id {
                creations.borrow_mut().finish(&request_id);
                apply_pending_creations(ui, &creations.borrow());
            }
            apply_import_submission_finished(ui, import_flow, generation, succeeded);
        }
        Event::StorageRootPicked(path) => {
            ui.set_persistent_root(path.display().to_string().into());
        }
        Event::ServerPackPicked { target, path } => {
            set_server_pack(ui, target, Some(&path));
        }
        Event::WatchedFolderPicked(path) => {
            let mut folders: Vec<String> = ui
                .get_watched_download_folders()
                .iter()
                .map(|folder| folder.to_string())
                .collect();
            let picked = path.display().to_string();
            if !folders.contains(&picked) {
                folders.push(picked);
            }
            send(commands, Command::SaveWatchedDownloadFolders(folders));
        }
    }
}

/// Re-renders the import modal from `applied`, the result of applying one of
/// the import flow's generation-gated transitions. A late-arriving result
/// this flow no longer tracks does not reach here at all (`applied` is
/// `false`), and one that does still must not clobber a different modal the
/// user has since navigated to.
fn apply_import_result(ui: &AppWindow, import_flow: &Rc<RefCell<ImportFlowState>>, applied: bool) {
    if applied && ui.get_modal() == "import" {
        render_import_flow(ui, &import_flow.borrow());
    }
}

fn apply_import_submission_finished(
    ui: &AppWindow,
    import_flow: &Rc<RefCell<ImportFlowState>>,
    generation: u64,
    succeeded: bool,
) {
    let applied = import_flow.borrow_mut().submit_finished(generation);
    match import_submission_action(ui.get_modal().as_str(), applied, succeeded) {
        ImportSubmissionAction::Ignore => {}
        ImportSubmissionAction::Close => {
            ui.set_modal(SharedString::default());
            ui.set_modal_error(SharedString::default());
        }
        ImportSubmissionAction::Render => {
            render_import_flow(ui, &import_flow.borrow());
        }
    }
}

/// What to do with the currently open modal after an `ImportPack` submission
/// reaches a terminal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportSubmissionAction {
    /// Either the outcome belongs to an attempt this flow no longer tracks
    /// (`applied` is `false`), or the user has since navigated away from the
    /// import modal entirely: nothing here should touch what is open now.
    Ignore,
    /// The current attempt's own submission succeeded while its modal is
    /// still open.
    Close,
    /// The current attempt's own submission ended without succeeding
    /// (failure, cancellation, or a setup failure) while its modal is still
    /// open: the guard already released, so re-render to show it.
    Render,
}

/// Pure so the ownership rule ("only the attempt's own still-open modal may
/// be closed by its own success") is checkable without constructing a live
/// `AppWindow`, which nothing in this crate's tests does.
fn import_submission_action(
    current_modal: &str,
    applied: bool,
    succeeded: bool,
) -> ImportSubmissionAction {
    if !applied || current_modal != "import" {
        return ImportSubmissionAction::Ignore;
    }
    if succeeded {
        ImportSubmissionAction::Close
    } else {
        ImportSubmissionAction::Render
    }
}

/// Selects one instance and puts its card in front of the user.
///
/// The list can be filtered or searched down to something the new instance is
/// not in, and a selection nothing shows is no selection at all, so a filter
/// that would hide it is cleared rather than left to hide it.
fn reveal_instance(ui: &AppWindow, instance: &str) {
    ui.set_selected_instance_id(instance.into());
    ui.set_instance_filter("all".into());
    ui.set_instance_query(SharedString::default());
    ui.invoke_select_instance(instance.into());
    // Where the card sits is not known until the list holding it has arrived,
    // so the scroll waits for the snapshot rather than happening here.
    ui.set_pending_reveal_instance(instance.into());
    ui.invoke_refresh();
}

/// Records the saved favorites and reorders whatever release list is on screen.
///
/// The list itself is not refetched: reordering is the whole effect, and a
/// favorite added while the New Instance dialog is open should move a row, not
/// blank the dialog while a catalog is asked again.
/// Records the saved favorites and lights the star for the release on screen.
///
/// Deliberately does not reorder the release list. A `ComboBox` rebuilds its
/// selection whenever its model changes: it clamps the index it was holding and
/// takes whatever now sits there, and Slint documents that writing the value
/// back cannot move it. Reordering under an open dialog therefore silently
/// changed which release the user had chosen. The new order is applied when the
/// list is next loaded, which is what opening the dialog does.
fn apply_favorite_versions(ui: &AppWindow, favorites: Vec<String>) {
    ui.set_create_minecraft_favorite(favorites.contains(&ui.get_create_minecraft().to_string()));
    ui.set_favorite_minecraft_versions(model(
        favorites.into_iter().map(SharedString::from).collect(),
    ));
}

fn apply_minecraft_releases(ui: &AppWindow, versions: Vec<String>) {
    // Favorites first, so the default selection below lands on one of them
    // rather than on whatever Mojang published most recently.
    let versions = presentation::releases_with_favorites_first(versions, &favorite_versions(ui));
    if !versions.is_empty() {
        let create_current = ui.get_create_minecraft();
        let server_current = ui.get_create_server_minecraft();
        if !versions
            .iter()
            .any(|version| version == create_current.as_str())
        {
            ui.set_create_minecraft(versions[0].clone().into());
        }
        if !versions
            .iter()
            .any(|version| version == server_current.as_str())
        {
            ui.set_create_server_minecraft(versions[0].clone().into());
        }
    }
    ui.set_minecraft_releases(model(
        versions.into_iter().map(SharedString::from).collect(),
    ));
    // The selection above may have moved, and both which loaders exist and
    // whether the release is starred depend on it.
    apply_minecraft_selection(ui, "client", ui.get_create_minecraft().as_ref());
    apply_minecraft_selection(ui, "server", ui.get_create_server_minecraft().as_ref());
}

/// Refreshes the name the dialog suggests for the release and loader currently
/// selected.
///
/// Only the suggestion: it is shown as the field's placeholder, never written
/// into it, so recomputing is always safe and a name the user typed is never
/// touched.
/// The name a new server takes when nobody types one.
///
/// A linked server's suggestion is its client's own name and is set when the
/// dialog opens, so this leaves it alone: there is no exact target on screen to
/// derive one from, and overwriting it would replace a real name with a version
/// string.
fn recompute_create_server_name(ui: &AppWindow) {
    if !ui.get_create_server_client_id().is_empty() {
        return;
    }
    let loader = loader_kind_of(ui.get_create_server_loader().as_str())
        .unwrap_or(ephervex_model::LoaderKind::Vanilla);
    let loader_version = ui.get_create_server_loader_version();
    let loader_version = (!loader_version.is_empty()).then(|| loader_version.as_str());
    let target = ephervex_model::default_instance_name(
        ui.get_create_server_minecraft().as_str(),
        loader,
        loader_version,
    );
    // An unnamed target suggests nothing rather than the bare word, which would
    // create a server called "Server".
    let suggestion = if target.is_empty() {
        String::new()
    } else {
        format!("{target} Server")
    };
    ui.set_create_server_name_suggestion(suggestion.into());
}

fn recompute_create_name(ui: &AppWindow) {
    let loader = loader_kind_of(ui.get_create_loader().as_str())
        .unwrap_or(ephervex_model::LoaderKind::Vanilla);
    let loader_version = ui.get_create_loader_version();
    let loader_version = (!loader_version.is_empty()).then(|| loader_version.as_str());
    ui.set_create_name_suggestion(
        ephervex_model::default_instance_name(
            ui.get_create_minecraft().as_str(),
            loader,
            loader_version,
        )
        .into(),
    );
}

fn apply_loader_versions(
    ui: &AppWindow,
    form: &str,
    loader: ephervex_model::LoaderKind,
    minecraft: &str,
    versions: &[String],
) {
    let label = loader.to_string();
    // The loader-change form asks for one instance's own family and release, so
    // there is nothing on screen to compare against and nothing to discard.
    if form == "loader-change" {
        ui.set_loader_change_versions(model(
            versions.iter().cloned().map(SharedString::from).collect(),
        ));
        ui.set_loader_change_loading(false);
        if ui.get_loader_change_selected().is_empty() {
            ui.set_loader_change_selected(
                ui.get_loader_change_current().as_str().to_owned().into(),
            );
        }
        return;
    }
    let current_matches = if form == "server" {
        ui.get_create_server_loader() == label && ui.get_create_server_minecraft() == minecraft
    } else {
        ui.get_create_loader() == label && ui.get_create_minecraft() == minecraft
    };
    if !current_matches {
        return;
    }
    let rows = model(versions.iter().cloned().map(SharedString::from).collect());
    if form == "server" {
        ui.set_create_server_loader_versions(rows);
        ui.set_create_server_loader_version(versions.first().cloned().unwrap_or_default().into());
        recompute_create_server_name(ui);
    } else {
        ui.set_create_loader_versions(rows);
        ui.set_create_loader_version(versions.first().cloned().unwrap_or_default().into());
        recompute_create_name(ui);
    }
}

fn pending_creation(key: &CreationKey, persistent_import: bool) -> PendingCreation {
    let (name, target, loader_kind, persistent, server) = match key {
        CreationKey::Client {
            name,
            minecraft,
            loader,
            loader_version,
            persistent,
            ..
        } => (
            name.clone(),
            creation_target(minecraft, loader, loader_version),
            loader.to_ascii_lowercase(),
            *persistent,
            false,
        ),
        CreationKey::Import { archive } => (
            // The name is not known until the archive has been read, so the
            // card is titled by what the user chose.
            std::path::Path::new(archive).file_name().map_or_else(
                || archive.clone(),
                |name| name.to_string_lossy().into_owned(),
            ),
            lookup("gui-import-modpack"),
            "vanilla".to_owned(),
            persistent_import,
            false,
        ),
        CreationKey::Server {
            name,
            client,
            minecraft,
            loader,
            loader_version,
            persistent,
            ..
        } => (
            name.clone(),
            if client.is_empty() {
                creation_target(minecraft, loader, loader_version)
            } else {
                lookup("gui-linked-client")
            },
            if client.is_empty() {
                loader.to_ascii_lowercase()
            } else {
                "vanilla".to_owned()
            },
            *persistent,
            true,
        ),
    };
    PendingCreation {
        request_id: String::new(),
        key: key.clone(),
        name,
        target,
        loader_kind,
        storage: lookup(if persistent {
            "gui-persistent"
        } else {
            "gui-temporary"
        }),
        storage_kind: if persistent {
            "persistent"
        } else {
            "temporary"
        }
        .to_owned(),
        server,
        detail: String::new(),
        progress: 0.0,
        indeterminate: true,
        linked_client_id: match key {
            CreationKey::Server { client, .. } => client.clone(),
            CreationKey::Client { .. } | CreationKey::Import { .. } => String::new(),
        },
    }
}

fn creation_target(minecraft: &str, loader: &str, loader_version: &str) -> String {
    let loader = if loader_version.is_empty() {
        loader.to_owned()
    } else {
        format!("{loader} {loader_version}")
    };
    lookup_args(
        "gui-target-summary",
        [
            ("first", Arg::from(minecraft)),
            ("second", Arg::from(loader)),
        ],
    )
}

fn apply_pending_creations(ui: &AppWindow, tracker: &CreationTracker) {
    let rows = |server: bool, global: bool| {
        tracker
            .pending
            .iter()
            .filter(|pending| {
                pending.server == server
                    && (!global
                        || ui.get_show_linked_servers()
                        || pending.linked_client_id.is_empty())
            })
            .map(|pending| PendingEntityRow {
                id: pending.request_id.clone().into(),
                name: pending.name.clone().into(),
                target: pending.target.clone().into(),
                loader_kind: pending.loader_kind.clone().into(),
                storage: pending.storage.clone().into(),
                storage_kind: pending.storage_kind.clone().into(),
                state: lookup("gui-state-creating").into(),
                server,
                linked_client_id: pending.linked_client_id.clone().into(),
                detail: pending.detail.clone().into(),
                progress: pending.progress,
                indeterminate: pending.detail.is_empty() || pending.indeterminate,
            })
            .collect::<Vec<_>>()
    };
    ui.set_pending_instances(model(rows(false, false)));
    ui.set_pending_servers(model(rows(true, true)));
    let selected_instance = ui.get_selected_instance_id();
    ui.set_linked_pending_servers(model(
        rows(true, false)
            .into_iter()
            .filter(|row| {
                !row.linked_client_id.is_empty() && row.linked_client_id == selected_instance
            })
            .collect(),
    ));
}

/// The non-secret provider identifiers: the Microsoft Client ID and the
/// advanced User-Agent override, both of which round-trip through the view.
fn register_provider_identifiers(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    ui.on_save_api_settings(move |client_id, user_agent| {
        send(
            &sender,
            Command::SaveApiSettings {
                client_id: client_id.into(),
                user_agent: user_agent.into(),
            },
        );
    });

    let sender = commands.clone();
    ui.on_reset_microsoft_client_id(move || send(&sender, Command::ResetMicrosoftClientId));
    let sender = commands.clone();
    ui.on_reset_user_agent(move || send(&sender, Command::ResetUserAgent));

    let sender = commands.clone();
    ui.on_choose_default_mod_catalog(move |catalog| {
        // An empty string is the reset action, and any other unparsable value
        // is dropped rather than turned into a catalog nobody picked.
        let chosen = if catalog.is_empty() {
            None
        } else {
            match ModProvider::parse(&catalog) {
                Some(provider) => Some(provider),
                None => return,
            }
        };
        // Nothing is written to the view here. The chips follow the stored
        // value, which arrives in the snapshot a successful write sends.
        send(&sender, Command::SaveDefaultModCatalog { catalog: chosen });
    });
}

/// The CurseForge credential actions: set or replace, remove, and apply for a
/// key.
///
/// Its own function because of the one rule that separates it from every other
/// setting: the input field is emptied on submit and is never written from a
/// snapshot, so a stored key has no path back into the window.
fn register_curseforge_credential(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_save_curseforge_api_key(move |key| {
        // Blank submissions are dropped here rather than sent, which is also
        // what makes a second click on an already-emptied field do nothing.
        if key.trim().is_empty() {
            return;
        }
        send(&sender, Command::SaveCurseForgeApiKey(key.into()));
        // Emptied the moment it is submitted, not when the write comes back:
        // the field must never hold a credential longer than it has to, and
        // nothing ever puts one back into it.
        if let Some(ui) = weak.upgrade() {
            ui.set_curseforge_api_key(SharedString::new());
        }
    });

    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_remove_curseforge_api_key(move || {
        send(&sender, Command::RemoveCurseForgeApiKey);
        // Removing does not need the existing value, and must not leave a
        // half-typed replacement behind either.
        if let Some(ui) = weak.upgrade() {
            ui.set_curseforge_api_key(SharedString::new());
        }
    });

    let sender = commands.clone();
    ui.on_open_curseforge_key_help(move || send(&sender, Command::OpenCurseForgeKeyHelp));
}

/// The three artifact-cache actions the storage settings offer.
fn register_artifact_cache(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    ui.on_measure_artifact_cache(move || send(&sender, Command::MeasureArtifactCache));
    let sender = commands.clone();
    ui.on_save_artifact_cache_limit(move |selection, custom_value, custom_unit| {
        send(
            &sender,
            Command::SaveArtifactCacheLimit {
                // A negative index can only mean the view has no entry selected,
                // which the typed resolution treats as a custom size - so the
                // fields beside it decide, rather than a preset nobody picked.
                selection: usize::try_from(selection).unwrap_or(usize::MAX),
                custom_value: custom_value.into(),
                custom_unit: custom_unit.into(),
            },
        );
    });
    let sender = commands.clone();
    ui.on_clear_artifact_cache(move || send(&sender, Command::ClearArtifactCache));
}

/// The budget selector, from the configuration file alone.
///
/// Deliberately separate from [`apply_artifact_cache_status`]: the limit is cheap
/// and arrives with every snapshot, while the usage figure costs a walk of the
/// cache and arrives only when something asked for one.
fn apply_artifact_cache_settings(ui: &AppWindow, settings: &SettingsData) {
    ui.set_artifact_cache_limit_labels(model(
        settings
            .artifact_cache_limit_labels
            .iter()
            .map(|label| SharedString::from(label.as_str()))
            .collect(),
    ));
    ui.set_artifact_cache_units(model(
        settings
            .artifact_cache_units
            .iter()
            .map(|label| SharedString::from(label.as_str()))
            .collect(),
    ));
    ui.set_artifact_cache_custom_index(index_for_view(settings.artifact_cache_custom_index));
    ui.set_artifact_cache_limit_index(index_for_view(settings.artifact_cache_limit_index));
    ui.set_artifact_cache_custom_value(settings.artifact_cache_custom_value.as_str().into());
    ui.set_artifact_cache_custom_unit(settings.artifact_cache_custom_unit.as_str().into());
    if !ui.get_artifact_cache_usage_known() {
        // Nothing has been measured yet, so the only honest total to show beside
        // "measuring" is the budget itself.
        ui.set_artifact_cache_total(settings.artifact_cache_limit_display.as_str().into());
    }
}

/// A list index as the view holds it.
///
/// Slint counts with `int`, so an index that somehow does not fit becomes the
/// "no such entry" value rather than wrapping onto a real one.
fn index_for_view(index: usize) -> i32 {
    i32::try_from(index).unwrap_or(-1)
}

fn apply_artifact_cache_status(ui: &AppWindow, status: ArtifactCacheStatusData) {
    ui.set_artifact_cache_used(status.used.into());
    ui.set_artifact_cache_total(status.total.into());
    ui.set_artifact_cache_usage_exact(status.exact);
    ui.set_artifact_cache_over_budget(status.over_budget);
    ui.set_artifact_cache_usage_known(true);
    // Whatever the clear did, it is finished: the figure above is the answer.
    ui.set_artifact_cache_clearing(false);
}

fn apply_snapshot(
    ui: &AppWindow,
    snapshot: Snapshot,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
) {
    let Snapshot {
        instances,
        servers,
        accounts,
        minecraft_preferences,
        settings,
        eula,
    } = snapshot;
    ui.set_show_linked_servers(settings.show_linked_servers);
    apply_favorite_versions(ui, settings.favorite_minecraft_versions.clone());
    ui.set_make_default_account_operator(settings.make_default_account_operator);
    ui.set_operator_permission_level(i32::from(settings.operator_permission_level));
    apply_instance_snapshot(ui, instances);
    apply_server_snapshot(ui, servers);
    ui.set_accounts(model(account_rows(accounts)));
    // A refresh happens for reasons unrelated to this screen, so the model
    // folds the confirmed state under whatever is unsaved rather than over it.
    minecraft.borrow_mut().apply_snapshot(minecraft_preferences);
    render_minecraft(ui, minecraft);
    apply_artifact_cache_settings(ui, &settings);
    ui.set_storage_root(settings.storage_root.into());
    ui.set_persistent_root(settings.persistent_root.into());
    ui.set_default_persistent_root(settings.default_persistent_root.into());
    ui.set_persistent_root_is_custom(settings.persistent_root_origin.is_override());
    ui.set_watched_download_folders(model(
        settings
            .watched_download_folders
            .into_iter()
            .map(SharedString::from)
            .collect(),
    ));
    ui.set_microsoft_client_id(settings.microsoft_client_id.into());
    ui.set_effective_microsoft_client_id(settings.effective_microsoft_client_id.into());
    ui.set_microsoft_client_id_is_custom(settings.microsoft_client_id_origin.is_override());
    ui.set_user_agent(settings.user_agent.into());
    ui.set_effective_user_agent(settings.effective_user_agent.into());
    ui.set_user_agent_is_custom(settings.user_agent_origin.is_override());
    // Presence only. There is deliberately no `set_curseforge_api_key` here:
    // a snapshot must not be able to put a stored secret into a text field.
    ui.set_curseforge_key_configured(settings.curseforge_key_present);
    ui.set_default_mod_catalog(settings.default_mod_catalog.clone().into());
    ui.set_configured_mod_catalog(settings.configured_mod_catalog.clone().into());
    ui.set_default_mod_catalog_is_custom(settings.mod_catalog_origin.is_override());
    ui.set_application_text_scale(i32::from(settings.text_scale_percent));
    ui.set_technical_text_size(i32::from(settings.technical_text_size_pixels));
    ui.global::<Theme>()
        .set_text_scale(f32::from(settings.text_scale_percent) / 100.0);
    ui.set_java_auto_detect(settings.java_auto_detect);
    ui.set_java_auto_download(settings.java_auto_download);
    ui.set_java_memory_min(settings.java_memory_min.to_string().into());
    ui.set_java_memory_max(settings.java_memory_max.to_string().into());
    ui.set_java_preferred_path(settings.java_preferred_path.into());
    ui.set_eula_accepted(eula.accepted);
    ui.set_eula_status(eula.status.into());
    ui.set_loading(false);
    ui.set_status_message(lookup("gui-ready").into());
}

fn apply_instance_snapshot(
    ui: &AppWindow,
    instances: Vec<crate::presentation::InstancePresentation>,
) {
    let previous_instance = ui.get_selected_instance_id();
    let mut instance_rows = instances
        .into_iter()
        .map(|item| InstanceRow {
            id: item.id.into(),
            name: item.name.into(),
            minecraft: item.minecraft.into(),
            loader: item.loader.into(),
            loader_kind: item.loader_kind.into(),
            storage: item.storage.into(),
            storage_kind: item.storage_kind.into(),
            source: item.source.into(),
            path: item.path.into(),
            updated: lookup("gui-available").into(),
            linked_servers: to_i32(item.linked_servers),
            mods: to_i32(item.managed_mods),
            needs_attention: item.needs_attention,
            state: item.state.into(),
            state_kind: item.state_kind.into(),
            can_launch: item.can_launch,
            can_kill: item.can_kill,
            connect_server_id: item
                .launch_and_connect
                .first()
                .map_or_else(SharedString::default, |connect| {
                    connect.server_id.clone().into()
                }),
            connect_server_name: item
                .launch_and_connect
                .first()
                .map_or_else(SharedString::default, |connect| {
                    connect.server_name.clone().into()
                }),
            can_connect: item
                .launch_and_connect
                .first()
                .is_some_and(|connect| connect.enabled),
        })
        .collect::<Vec<_>>();
    let storage_filter = ui.get_instance_filter();
    let query = ui.get_instance_query().to_ascii_lowercase();
    instance_rows.retain(|row| {
        (storage_filter == "all" || row.storage_kind == storage_filter)
            && row_matches(
                &query,
                [&row.name, &row.minecraft, &row.loader, &row.source],
            )
    });
    let instance_index = selected_index(&instance_rows, &previous_instance, |row| &row.id);
    if let Some(index) = instance_index {
        ui.set_selected_instance_id(instance_rows[index].id.clone());
        ui.set_selected_instance_index(to_i32(index));
    } else {
        ui.set_selected_instance_id(SharedString::default());
        ui.set_selected_instance_index(-1);
    }
    ui.set_instances(reconciled(&ui.get_instances(), instance_rows));
    // The list a pending reveal was waiting for. Cleared whether or not the
    // card turned up, so a request cannot outlive the list it was made against.
    let pending = ui.get_pending_reveal_instance();
    if !pending.is_empty() {
        ui.set_pending_reveal_instance(SharedString::default());
        if instance_index.is_some() && ui.get_selected_instance_id() == pending {
            ui.set_reveal_selected_instance(ui.get_reveal_selected_instance().saturating_add(1));
        }
    }
}

fn apply_server_snapshot(ui: &AppWindow, servers: Vec<crate::presentation::ServerPresentation>) {
    let previous_global_server = ui.get_selected_global_server_id();
    let server_rows = servers.into_iter().map(server_row).collect::<Vec<_>>();
    let mut global_rows = server_rows
        .iter()
        .filter(|row| server_is_globally_visible(row, ui.get_show_linked_servers()))
        .cloned()
        .collect::<Vec<_>>();
    let query = ui.get_server_query().to_ascii_lowercase();
    global_rows.retain(|row| {
        row_matches(
            &query,
            [
                &row.name,
                &row.minecraft,
                &row.loader,
                &row.relationship,
                &row.address,
            ],
        )
    });
    let global_index = global_selection_index(&global_rows, &previous_global_server);
    if let Some(index) = global_index {
        ui.set_selected_global_server_id(global_rows[index].id.clone());
        ui.set_selected_global_server_index(to_i32(index));
    } else {
        ui.set_selected_global_server_id(SharedString::default());
        ui.set_selected_global_server_index(-1);
    }
    let selected_instance = ui.get_selected_instance_id();
    let linked_rows = server_rows
        .iter()
        .filter(|row| row.linked_client_id == selected_instance)
        .cloned()
        .collect::<Vec<_>>();
    let workspace_server = ui.get_selected_server_id();
    let workspace_index = server_rows
        .iter()
        .position(|row| row.id == workspace_server);
    ui.set_selected_server_index(workspace_index.map_or(-1, to_i32));
    ui.set_servers(reconciled(&ui.get_servers(), server_rows));
    ui.set_global_servers(reconciled(&ui.get_global_servers(), global_rows));
    ui.set_linked_servers(reconciled(&ui.get_linked_servers(), linked_rows));
}

fn server_is_globally_visible(row: &ServerRow, show_linked_servers: bool) -> bool {
    show_linked_servers || row.linked_client_id.is_empty()
}

fn global_selection_index(rows: &[ServerRow], previous: &str) -> Option<usize> {
    if previous.is_empty() {
        (!rows.is_empty()).then_some(0)
    } else {
        rows.iter().position(|row| row.id == previous)
    }
}

fn server_row(item: crate::presentation::ServerPresentation) -> ServerRow {
    ServerRow {
        id: item.id.into(),
        name: item.name.into(),
        minecraft: item.minecraft.into(),
        loader: item.loader.into(),
        loader_kind: item.loader_kind.into(),
        storage: item.storage.into(),
        storage_kind: item.storage_kind.into(),
        relationship: item.relationship.into(),
        linked_client_id: item.linked_client_id.into(),
        state: item.state.into(),
        state_kind: item.state_kind.into(),
        state_detail: item.state_detail.into(),
        address: item.address.into(),
        path: item.path.into(),
        installed: item.installed,
        needs_attention: item.needs_attention,
        can_start: item.can_start,
        can_stop: item.can_stop,
        can_restart: item.can_restart,
        can_kill: item.can_kill,
        can_send_command: item.can_send_command,
        can_connect: item.can_connect,
        connect_reason: item.connect_reason.into(),
    }
}

fn apply_server_workspace(ui: &AppWindow, workspace: ServerWorkspaceData) {
    ui.set_property_motd(workspace.motd.into());
    ui.set_property_max_players(workspace.max_players.into());
    ui.set_property_difficulty(workspace.difficulty.into());
    ui.set_property_gamemode(workspace.gamemode.into());
    ui.set_property_allow_flight(workspace.allow_flight);
    ui.set_property_pvp(workspace.pvp);
    ui.set_property_allowlist(workspace.allowlist);
    ui.set_property_online_mode(workspace.network.online_mode);
    ui.set_property_access(
        match workspace.network.access {
            ephervex_core::NetworkAccess::Localhost => "Localhost",
            ephervex_core::NetworkAccess::Lan => "LAN",
            ephervex_core::NetworkAccess::Custom => "Custom",
        }
        .into(),
    );
    ui.set_property_address(workspace.network.bind_address.unwrap_or_default().into());
    ui.set_property_port(workspace.network.port.to_string().into());
    ui.set_property_port_error(SharedString::default());
    ui.set_property_address_error(SharedString::default());
}

fn apply_diagnostic_content(ui: &AppWindow, content: DiagnosticContent) {
    ui.set_diagnostic_title(content.title.into());
    ui.set_diagnostic_path(content.path.into());
    ui.set_diagnostic_content(content.text.into());
    let status = match (content.truncated, content.lossy, content.matches) {
        (_, _, Some(matches)) => lookup_args(
            "gui-diagnostic-search-matches",
            [("count", Arg::from(matches))],
        ),
        (true, true, None) => lookup("gui-diagnostic-window-lossy"),
        (true, false, None) => lookup("gui-diagnostic-window-bounded"),
        (false, true, None) => lookup("gui-diagnostic-file-lossy"),
        (false, false, None) => lookup("gui-diagnostic-loaded"),
    };
    ui.set_status_message(status.into());
}

fn upsert_activity(activities: &mut Vec<ActivityData>, activity: ActivityData) {
    if let Some(existing) = activities
        .iter_mut()
        .find(|existing| existing.item.id == activity.item.id)
    {
        if !activity.entity.is_empty() {
            existing.entity = activity.entity;
        }
        existing.item = activity.item;
        existing.time = activity.time;
    } else {
        activities.insert(0, activity);
        activities.truncate(100);
    }
}

fn apply_activities(ui: &AppWindow, activities: &[ActivityData]) {
    let rows = activities
        .iter()
        .map(|activity| ActivityRow {
            id: activity.item.id.clone().into(),
            title: activity.item.title.clone().into(),
            entity: activity.entity.clone().into(),
            status: activity.item.status.clone().into(),
            detail: activity.item.detail.clone().into(),
            time: activity.time.clone().into(),
            progress: activity.item.progress,
            indeterminate: activity.item.indeterminate,
            active: matches!(activity.item.status_kind, "queued" | "running"),
            failed: activity.item.failed,
            cancellable: activity.item.cancellable,
        })
        .collect::<Vec<_>>();
    ui.set_active_operations(to_i32(rows.iter().filter(|row| row.active).count()));
    ui.set_activities(model(rows));
}

fn account_rows(rows: Vec<AccountData>) -> Vec<AccountRow> {
    rows.into_iter()
        .map(|row| AccountRow {
            id: row.id.into(),
            name: row.name.into(),
            kind: row.kind.into(),
            is_default: row.is_default,
            credential_state: row.credential_state.into(),
        })
        .collect()
}

/// Shows a mod's releases, unless the user has moved on from the mod that asked.
///
/// The same rule the managed-mod list follows: a background answer about
/// something no longer on screen is discarded rather than rendered against
/// whatever is.
fn apply_mod_versions(ui: &AppWindow, data: backend::ModVersionsData) {
    if !addresses_open_mod(ui, &data.instance, &data.logical_path) {
        return;
    }
    ui.set_mod_action_loading(false);
    ui.set_mod_action_installed(data.installed_version.into());
    ui.set_mod_action_notice(data.notice.into());
    ui.set_mod_action_can_change(data.can_change_version);
    ui.set_mod_action_can_update(data.can_update);
    ui.set_mod_action_can_remove(data.can_remove);
    ui.set_mod_action_up_to_date(data.up_to_date);
    ui.set_mod_action_project_url(data.project_url.into());
    if !data.title.is_empty() {
        ui.set_mod_action_name(data.title.into());
    }
    ui.set_mod_versions(model(mod_version_rows(data.versions)));
}

/// Shows the page of the search the user is currently waiting on.
///
/// An answer is discarded when it is for another instance, another catalog, a
/// query that has since been superseded, an offset the browser is not
/// currently waiting for a page at, or a request id other than the one this
/// exact request was minted. The first four can all agree between two
/// distinct requests (typing the same query again, or switching catalog away
/// and back before an old search returns and then asking the same thing
/// again), which is exactly why the fifth exists. Searches run on their own
/// threads and can finish in either order, so without all five a slow older
/// one could replace or truncate the results already shown.
fn apply_mod_results(
    ui: &AppWindow,
    results: backend::ModResultsData,
    browser: &Rc<ModBrowser>,
    commands: &Sender<Command>,
) {
    let selected = ui.get_selected_instance_id();
    let submitted = ui.get_mod_search_submitted();
    let catalog = ui.get_mod_search_provider();
    let expected_offset = u32::try_from(ui.get_mod_search_requested_offset()).unwrap_or(0);
    let expected_request_id = ui.get_mod_search_expected_request_id();
    if !presentation::search_page_belongs(
        presentation::BrowserQuestion {
            instance: &selected,
            provider: &catalog,
            reference: &submitted,
        },
        presentation::BrowserQuestion {
            instance: &results.instance,
            provider: &results.provider,
            reference: &results.query,
        },
        results.offset,
        expected_offset,
        &results.request_id.to_string(),
        &expected_request_id,
    ) {
        return;
    }

    ui.set_mod_search_notice(results.notice.clone().into());
    if !results.notice.is_empty() {
        ui.set_mod_search_loading(false);
        return;
    }

    let mut rows: Vec<ModSearchRow> = ui.get_mod_results().iter().collect();
    rows.extend(mod_search_rows(results.rows));
    let shown = rows.len();
    ui.set_mod_results_total(to_i32(usize::try_from(results.total).unwrap_or(usize::MAX)));
    ui.set_mod_results(model(rows));
    mark_installed_results(ui);
    mark_queued_results(ui, &browser.installs.borrow());
    // The provider's own continuation, never how many rows are now on screen:
    // a page can consume results this launcher declined to show.
    let next_offset = results.next_offset.map_or(-1, |offset| {
        to_i32(usize::try_from(offset).unwrap_or(usize::MAX))
    });
    ui.set_mod_results_next_offset(next_offset);
    // A page size larger than one provider request is filled by asking again
    // rather than by asking for more at once: the ceiling on a single request
    // is a budget, because deciding compatibility costs one version lookup per
    // hit. Each continuation is an ordinary request with its own id, so the
    // staleness rules above hold for it exactly as they do for the first.
    if shown < result_target(ui)
        && next_offset >= 0
        && let Some(instance) = backend::parse_instance_id(&results.instance)
        && let Some(catalog) = ModProvider::parse(&results.provider)
        && let Some(request_id) = next_mod_search_request_id(&browser.search_requests)
    {
        ui.set_mod_search_requested_offset(next_offset);
        ui.set_mod_search_expected_request_id(request_id.to_string().into());
        send(
            commands,
            Command::SearchMods {
                instance,
                provider: catalog,
                query: results.query,
                offset: u32::try_from(next_offset).unwrap_or(0),
                limit: request_limit(),
                request_id,
            },
        );
        return;
    }
    ui.set_mod_search_loading(false);
}

/// The largest page a single provider request may take, which is the launcher's
/// budget rather than the user's choice.
fn request_limit() -> u8 {
    ephervex_core::MAX_SEARCH_LIMIT
}

/// How many rows the fill currently under way is aiming for.
///
/// Clamped rather than trusted: it is derived from a picker, and a fill aiming
/// at more than the largest offered page size would keep asking for pages.
fn result_target(ui: &AppWindow) -> usize {
    usize::try_from(ui.get_mod_results_target())
        .unwrap_or(usize::from(ephervex_core::DEFAULT_SEARCH_LIMIT))
        .clamp(1, presentation::MAX_RESULT_PAGE_SIZE.saturating_mul(100))
}

/// Shows what installing a mod would involve, here and on each linked server.
/// What a finished plan should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanDisposition {
    /// Apply it now, without asking.
    Install,
    /// Everything it would install is already there.
    NothingToDo,
    /// Show the confirmation, because something is left for the user to decide.
    Confirm,
}

/// Whether a plan can be applied without a confirmation.
///
/// A plan the browser's Install button asked for already has its answer: the
/// button said what it does, core resolved the release and everything it
/// requires, and repeating that back adds nothing. A plan the user reached any
/// other way is always confirmed.
///
/// Linked servers are the one thing a button press cannot answer. Which of them
/// should also receive the mod is a choice, and a choice needs the panel that
/// offers it.
fn plan_disposition(direct: bool, plan: &backend::ModPlanData) -> PlanDisposition {
    if !direct || !plan.linked.targets.is_empty() {
        return PlanDisposition::Confirm;
    }
    if plan.install_count == 0 {
        return PlanDisposition::NothingToDo;
    }
    PlanDisposition::Install
}

fn apply_mod_install_plan(
    ui: &AppWindow,
    plan: backend::ModPlanData,
    browser: &Rc<ModBrowser>,
    commands: &Sender<Command>,
) {
    // Whether the queue is waiting on this exact answer, decided before
    // anything below can discard it: a plan that is dropped still ends the
    // request that asked for it, or everything behind it waits forever.
    let awaited = BrowserInstall {
        instance: plan.instance.clone(),
        provider: plan.provider.clone(),
        project: plan.requested.clone(),
    };
    let queued = browser.installs.borrow().active.as_ref() == Some(&awaited);
    let retire = |ui: &AppWindow| {
        if queued {
            let next = browser.installs.borrow_mut().finish(&awaited);
            advance_browser_install(ui, commands, browser, next);
        }
    };

    if !presentation::result_is_current(&plan.instance, &ui.get_selected_instance_id()) {
        retire(ui);
        return;
    }
    // A confirmation arriving over a modal the user opened while it was being
    // planned would replace what they are looking at, and would leave the
    // linked-server rows describing a different operation than the one on
    // screen.
    let modal = ui.get_modal();
    if !modal.is_empty() && modal != "mod-plan" {
        retire(ui);
        return;
    }
    // Two reviews can be in flight; the older one must not replace the newer,
    // and neither may one for the same project id on the other catalog.
    let selected = ui.get_selected_instance_id();
    let requested = ui.get_mod_plan_requested();
    let catalog = ui.get_mod_plan_requested_provider();
    if !presentation::browser_answer_is_current(
        presentation::BrowserQuestion {
            instance: &selected,
            provider: &catalog,
            reference: &requested,
        },
        presentation::BrowserQuestion {
            instance: &plan.instance,
            provider: &plan.provider,
            reference: &plan.requested,
        },
    ) {
        retire(ui);
        return;
    }
    let direct = ui.get_mod_plan_direct();
    ui.set_mod_plan_direct(false);
    match plan_disposition(direct, &plan) {
        PlanDisposition::Install => {
            // The queue is retired by this install's own completion, not here:
            // the next one may not start while this one is writing the recipe.
            ui.set_mod_action_pending(true);
            // Nothing was offered, so nothing was chosen. Submitting the rows
            // still in the model would act on servers this plan never named.
            ui.set_linked_targets(model(Vec::new()));
            ui.invoke_install_mod(
                plan.instance.into(),
                plan.provider.into(),
                plan.project.into(),
                plan.version_id.into(),
            );
            return;
        }
        PlanDisposition::NothingToDo => {
            ui.invoke_present_toast(lookup("gui-mod-already-present").into(), false);
            retire(ui);
            return;
        }
        // The confirmation owns the request until it is answered or closed.
        PlanDisposition::Confirm => {}
    }
    ui.set_mod_plan_instance(plan.instance.into());
    ui.set_mod_plan_provider(plan.provider.into());
    ui.set_mod_plan_project(plan.project.into());
    ui.set_mod_plan_title(plan.title.into());
    ui.set_mod_plan_version(plan.version.into());
    ui.set_mod_plan_version_id(plan.version_id.into());
    ui.set_mod_plan_install_count(to_i32(plan.install_count));
    ui.set_mod_plan_dependency_count(to_i32(plan.dependency_count));
    ui.set_mod_plan_optional_count(to_i32(plan.optional_count));
    ui.set_mod_plan_existing_count(to_i32(plan.already_installed_count));
    ui.set_mod_plan_warning_count(to_i32(plan.warning_count));
    apply_linked_section(ui, plan.linked);
    ui.set_mod_action_pending(false);
    ui.set_modal("mod-plan".into());
    ui.set_modal_error(SharedString::default());
}

/// Shows what one finished operation did to each target that took part.
fn apply_linked_result(ui: &AppWindow, result: backend::LinkedResultData) {
    let needs_review =
        presentation::linked_result_needs_review(result.rows.iter().map(|row| row.tone.as_str()));
    let summary = result.summary.clone();
    ui.set_linked_result_title(result.title.into());
    ui.set_linked_result_summary(result.summary.into());
    ui.set_linked_results(model(linked_target_rows(result.rows)));
    ui.set_mod_action_pending(false);
    ui.set_modal_error(SharedString::default());
    // An operation that reached every target it was asked to says so in one
    // line. The per-target report is opened only when a target did not take
    // it, which is the case a reader has to act on.
    if needs_review {
        ui.set_modal("mod-result".into());
    } else {
        ui.set_modal(SharedString::default());
        ui.invoke_present_toast(summary.into(), false);
    }
}

/// Shows what a removal would take with it, here and on each linked server.
fn apply_mod_removal_plan(ui: &AppWindow, data: backend::ModRemovalPlanData) {
    if !addresses_open_mod(ui, &data.instance, &data.logical_path) {
        return;
    }
    ui.set_mod_action_name(data.name.into());
    ui.set_mod_removal_unprovable(data.dependencies_unprovable);
    ui.set_mod_removal_dependencies(model(
        data.dependencies
            .into_iter()
            .map(SharedString::from)
            .collect(),
    ));
    apply_linked_section(ui, data.linked);
    ui.set_mod_action_pending(false);
    ui.set_modal("mod-remove".into());
}

/// Shows the exact release a change or update settled on, and which linked
/// servers can take it.
fn apply_linked_confirmation(ui: &AppWindow, section: backend::LinkedSectionData) {
    if !addresses_open_mod(ui, &section.instance, &section.path) {
        return;
    }
    apply_linked_section(ui, section);
    ui.set_mod_action_pending(false);
    ui.set_modal_error(SharedString::default());
    ui.set_modal("mod-apply".into());
}

/// Puts one confirmation's linked-server half on screen.
fn apply_linked_section(ui: &AppWindow, section: backend::LinkedSectionData) {
    ui.set_linked_path(section.path.into());
    ui.set_linked_version(section.version.into());
    ui.set_linked_name(section.name.into());
    ui.set_linked_version_number(section.version_number.into());
    ui.set_linked_support(section.support.into());
    ui.set_linked_targets(model(linked_target_rows(section.targets)));
}

fn linked_target_rows(rows: Vec<backend::LinkedTargetData>) -> Vec<LinkedTargetRow> {
    rows.into_iter()
        .map(|row| LinkedTargetRow {
            id: row.id.into(),
            name: row.name.into(),
            detail: row.detail.into(),
            state: row.state.into(),
            explanation: row.explanation.into(),
            tone: row.tone.into(),
            actionable: row.actionable,
            selected: row.selected,
        })
        .collect()
}

/// Whether a background answer is about the mod the user currently has open.
fn addresses_open_mod(ui: &AppWindow, instance: &str, logical_path: &str) -> bool {
    presentation::result_is_current(instance, &ui.get_selected_instance_id())
        && logical_path == ui.get_mod_action_path().as_str()
}

/// The loader a display label or kind names, or `None` for vanilla.
///
/// One reading of both spellings the UI carries: the product name shown in a
/// form, and the lowercase kind a row carries for its accent colour.
fn loader_kind_of(value: &str) -> Option<ephervex_model::LoaderKind> {
    match value {
        "Forge" | "forge" => Some(ephervex_model::LoaderKind::Forge),
        "NeoForge" | "neoforge" => Some(ephervex_model::LoaderKind::NeoForge),
        _ => None,
    }
}

/// The title the mod list is already showing for one row.
///
/// Used to name the modal before the catalog answers, so it opens with the mod
/// the user clicked rather than with a blank heading.
fn mod_row_title(ui: &AppWindow, path: &SharedString) -> SharedString {
    ui.get_managed_mods()
        .iter()
        .find(|row| row.logical_path == *path)
        .map_or_else(SharedString::default, |row| row.title.clone())
}

fn mod_version_rows(rows: Vec<backend::ModVersionData>) -> Vec<ModVersionRow> {
    rows.into_iter()
        .map(|row| ModVersionRow {
            version_id: row.version_id.into(),
            name: row.name.into(),
            number: row.number.into(),
            channel: row.channel.into(),
            published: row.published.into(),
            installed: row.installed,
            newest: row.newest,
        })
        .collect()
}

fn managed_mod_rows(rows: Vec<ManagedModData>) -> Vec<ModRow> {
    rows.into_iter()
        .map(|row| ModRow {
            project_id: row.project_id.into(),
            logical_path: row.logical_path.into(),
            can_manage: row.can_manage,
            can_change_version: row.can_change_version,
            can_update: row.can_update,
            can_remove: row.can_remove,
            can_delete_file: row.can_delete_file,
            restriction: row.restriction.into(),
            title: row.title.into(),
            file_name: row.file_name.into(),
            version: row.version.into(),
            version_known: row.version_known,
            provider: row.provider.into(),
            provider_kind: row.provider_kind.into(),
            origin: row.origin.into(),
            origin_kind: row.origin_kind.into(),
            compatibility: row.compatibility.into(),
            author: row.author.into(),
            icon: cached_image(&row.icon),
            enabled: row.enabled,
            deleted: row.deleted,
            pack_owned: row.pack_owned,
            suspect_reason: row.suspect_reason.into(),
            suspect_confidence: row.suspect_confidence.into(),
        })
        .collect()
}

/// Marks the results this instance already has.
///
/// Decided against the managed-mods list rather than remembered from a press,
/// so a mod installed from anywhere — this browser, a pack import, a linked
/// server operation — is shown as installed without a re-search, and one
/// removed stops being.
fn mark_installed_results(ui: &AppWindow) {
    let managed: std::collections::HashSet<SharedString> = ui
        .get_managed_mods()
        .iter()
        .map(|row| row.project_id)
        .filter(|project| !project.is_empty())
        .collect();
    let rows = ui
        .get_mod_results()
        .iter()
        .map(|row| ModSearchRow {
            installed: managed.contains(&row.project_id),
            ..row
        })
        .collect::<Vec<_>>();
    ui.set_mod_results(model(rows));
}

/// Marks the results whose install is queued or under way.
fn mark_queued_results(ui: &AppWindow, queue: &BrowserInstallQueue) {
    let instance = ui.get_selected_instance_id();
    let provider = ui.get_mod_provider();
    let outstanding: std::collections::HashSet<&str> = queue
        .outstanding(instance.as_str(), provider.as_str())
        .into_iter()
        .collect();
    let rows = ui
        .get_mod_results()
        .iter()
        .map(|row| ModSearchRow {
            queued: outstanding.contains(row.project_id.as_str()),
            ..row
        })
        .collect::<Vec<_>>();
    ui.set_mod_results(model(rows));
}

/// Loads an image the application service already put in its cache.
///
/// The path comes from `ephervex-core`, which is the only thing that decides
/// what may be fetched and where it lands; an empty path means there is no
/// artwork, and an unreadable file is the same thing to a list that has a
/// fallback. Neither is worth interrupting the user over.
fn cached_image(path: &str) -> Image {
    if path.is_empty() {
        return Image::default();
    }
    Image::load_from_path(Path::new(path)).unwrap_or_else(|error| {
        tracing::debug!(%path, ?error, "cached mod artwork could not be decoded");
        Image::default()
    })
}

fn mod_search_rows(rows: Vec<ModSearchData>) -> Vec<ModSearchRow> {
    rows.into_iter()
        .map(|row| ModSearchRow {
            project_id: row.project_id.into(),
            title: row.title.into(),
            description: row.description.into(),
            author: row.author.into(),
            note: row.note.into(),
            icon: cached_image(&row.icon),
            // Both are decided against state the row cannot see, so a fresh
            // page arrives unmarked and is marked by the two passes below.
            installed: false,
            queued: false,
        })
        .collect()
}

fn diagnostic_rows(rows: Vec<DiagnosticData>) -> Vec<DiagnosticRow> {
    rows.into_iter()
        .map(|row| DiagnosticRow {
            scope_kind: row.scope_kind.into(),
            scope_id: row.scope_id.into(),
            entity: row.entity.into(),
            kind: row.kind.into(),
            name: row.name.into(),
            path: row.path.into(),
            modified: row.modified.into(),
            size: row.size.into(),
            summary: row.summary.into(),
        })
        .collect()
}

fn java_rows(rows: Vec<JavaData>) -> Vec<JavaRow> {
    rows.into_iter()
        .map(|row| JavaRow {
            path: row.path.into(),
            version: row.version.into(),
            vendor: row.vendor.into(),
            origin: row.origin.into(),
            compatible: row.compatible.into(),
        })
        .collect()
}

fn selected_index<T>(
    rows: &[T],
    previous: &SharedString,
    id: impl Fn(&T) -> &SharedString,
) -> Option<usize> {
    if previous.is_empty() {
        (!rows.is_empty()).then_some(0)
    } else {
        rows.iter()
            .position(|row| id(row) == previous)
            .or_else(|| (!rows.is_empty()).then_some(0))
    }
}

fn row_matches<'a>(query: &str, values: impl IntoIterator<Item = &'a SharedString>) -> bool {
    query.is_empty()
        || values
            .into_iter()
            .any(|value| value.to_ascii_lowercase().contains(query))
}

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}

/// The model to store for a list, reusing the one already there when it can.
///
/// Handing a repeater a different `ModelRc` makes it tear down and rebuild every
/// row component, and with them whatever those components were in the middle of.
/// A half-finished double click is exactly that: the second press lands on a
/// freshly built touch area that never saw the first, so it reads as two single
/// clicks. Snapshots arrive for reasons that have nothing to do with the list
/// changing, so the rows have to survive one.
///
/// Rows are therefore written into the existing model, and only where they
/// actually differ. An unchanged list produces no notifications at all.
fn reconciled<T: Clone + PartialEq + 'static>(list: &ModelRc<T>, rows: Vec<T>) -> ModelRc<T> {
    let Some(existing) = list.as_any().downcast_ref::<VecModel<T>>() else {
        return model(rows);
    };
    while existing.row_count() > rows.len() {
        existing.remove(existing.row_count() - 1);
    }
    for (index, row) in rows.into_iter().enumerate() {
        if index < existing.row_count() {
            if existing.row_data(index).as_ref() != Some(&row) {
                existing.set_row_data(index, row);
            }
        } else {
            existing.push(row);
        }
    }
    list.clone()
}

fn send(sender: &Sender<Command>, command: Command) {
    if sender.send(command).is_err() {
        tracing::error!("the GUI core worker stopped unexpectedly");
    }
}

fn to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use crate::presentation::ActivityPresentation;
    use ephervex_core::ArtifactCacheLimit;
    use ephervex_i18n::has;

    use super::*;

    /// A window on the testing backend.
    ///
    /// The backend is process-wide and installs once, so a second attempt from
    /// another test in the same binary is the ordinary outcome rather than a
    /// failure.
    fn test_window() -> AppWindow {
        let _ =
            slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
                i_slint_backend_testing::TestingBackendOptions {
                    threading: false,
                    ..Default::default()
                },
            )));
        AppWindow::new().expect("window")
    }

    /// A standalone server is named for its exact target, the same way a new
    /// instance is, with "Server" after it.
    #[test]
    fn a_standalone_servers_suggested_name_is_the_instance_default_plus_server() {
        let ui = test_window();
        ui.set_create_server_client_id(SharedString::default());

        ui.set_create_server_minecraft("1.21.1".into());
        ui.set_create_server_loader("Vanilla".into());
        ui.set_create_server_loader_version(SharedString::default());
        recompute_create_server_name(&ui);
        assert_eq!(ui.get_create_server_name_suggestion(), "1.21.1 Server");

        ui.set_create_server_loader("NeoForge".into());
        ui.set_create_server_loader_version("21.1.77".into());
        recompute_create_server_name(&ui);
        assert_eq!(
            ui.get_create_server_name_suggestion(),
            "1.21.1-NeoForge-21.1.77 Server"
        );
    }

    /// The same name a new instance would take, so the two dialogs cannot drift
    /// apart in what they call the same target.
    #[test]
    fn the_server_suggestion_tracks_the_instance_suggestion() {
        let ui = test_window();
        ui.set_create_minecraft("1.20.1".into());
        ui.set_create_loader("Forge".into());
        ui.set_create_loader_version("47.3.0".into());
        recompute_create_name(&ui);

        ui.set_create_server_client_id(SharedString::default());
        ui.set_create_server_minecraft("1.20.1".into());
        ui.set_create_server_loader("Forge".into());
        ui.set_create_server_loader_version("47.3.0".into());
        recompute_create_server_name(&ui);

        assert_eq!(
            ui.get_create_server_name_suggestion(),
            format!("{} Server", ui.get_create_name_suggestion())
        );
    }

    /// Nothing chosen yet suggests nothing, rather than a server called
    /// "Server".
    #[test]
    fn an_unchosen_target_suggests_no_name_at_all() {
        let ui = test_window();
        ui.set_create_server_client_id(SharedString::default());
        ui.set_create_server_minecraft(SharedString::default());
        ui.set_create_server_loader("Vanilla".into());

        recompute_create_server_name(&ui);

        assert_eq!(ui.get_create_server_name_suggestion(), "");
    }

    /// A linked server borrows its client's own name, which the dialog sets
    /// when it opens. Recomputing must not replace it with a version string.
    #[test]
    fn a_linked_servers_suggestion_is_left_to_the_client_that_named_it() {
        let ui = test_window();
        ui.set_create_server_client_id("repro-client".into());
        ui.set_create_server_name_suggestion("Caveopolis 2 Server".into());
        ui.set_create_server_minecraft("1.21.1".into());
        ui.set_create_server_loader("NeoForge".into());

        recompute_create_server_name(&ui);

        assert_eq!(
            ui.get_create_server_name_suggestion(),
            "Caveopolis 2 Server"
        );
    }

    fn mod_row(title: &str, file_name: &str, provider: &str) -> ModRow {
        ModRow {
            title: title.into(),
            file_name: file_name.into(),
            provider: provider.into(),
            version: "1.0.0".into(),
            ..ModRow::default()
        }
    }

    /// The box narrows the list by whatever the user remembers of a mod, which
    /// is rarely the start of its name.
    #[test]
    fn the_filter_matches_any_part_of_a_name_or_file_name() {
        let rows = vec![
            mod_row("Just Enough Items", "jei-1.21.1.jar", "CurseForge"),
            mod_row("Sodium", "sodium-fabric.jar", "Modrinth"),
        ];

        assert_eq!(matching_mods(&rows, "enough").len(), 1);
        assert_eq!(matching_mods(&rows, "ENOUGH").len(), 1, "case is ignored");
        assert_eq!(matching_mods(&rows, "jei").len(), 1, "the file name counts");
        assert_eq!(
            matching_mods(&rows, "modrinth").len(),
            1,
            "so does the catalog a row came from"
        );
        assert!(matching_mods(&rows, "nothing here").is_empty());
    }

    /// Clearing the box restores the list rather than leaving it empty.
    #[test]
    fn an_empty_filter_matches_everything() {
        let rows = vec![
            mod_row("Just Enough Items", "jei.jar", "CurseForge"),
            mod_row("Sodium", "sodium.jar", "Modrinth"),
        ];

        assert_eq!(matching_mods(&rows, "").len(), 2);
        assert_eq!(
            matching_mods(&rows, "   ").len(),
            2,
            "and so does whitespace"
        );
    }

    /// A query typed against one instance must not be left filtering the mods
    /// of the next one the user opens.
    #[test]
    fn changing_host_is_reported_so_the_query_can_be_cleared() {
        let mut lists = ModLists::default();

        assert!(lists.replace("client", "instance-a", vec![mod_row("A", "a.jar", "")]));
        assert!(!lists.replace("client", "instance-a", vec![mod_row("A", "a.jar", "")]));
        assert!(lists.replace("client", "instance-b", Vec::new()));
        assert!(
            lists.replace("server", "server-a", Vec::new()),
            "the two lists track their hosts separately"
        );
    }

    /// A file dropped on the window carries no cursor position, so what it is
    /// for is decided by what the window is asking for. Every state the user
    /// can be in when they let go has to have an answer.
    #[test]
    fn a_drop_belongs_to_whichever_dialog_is_asking_for_an_archive() {
        let ui = test_window();

        ui.set_modal(SharedString::default());
        assert_eq!(drop_intent(&ui), DropIntent::Import);

        ui.set_modal("create-server".into());
        assert_eq!(
            drop_intent(&ui),
            DropIntent::ServerPack(ServerPackTarget::NewServer)
        );

        ui.set_modal("import".into());
        ui.set_import_create_server(false);
        assert_eq!(
            drop_intent(&ui),
            DropIntent::Import,
            "an import with no server wanted is still asking for the pack"
        );

        ui.set_import_create_server(true);
        assert_eq!(
            drop_intent(&ui),
            DropIntent::ServerPack(ServerPackTarget::Import),
            "the pack is already chosen by the time that toggle can be reached"
        );

        ui.set_modal("delete-server".into());
        assert_eq!(
            drop_intent(&ui),
            DropIntent::Ignored,
            "a file let go over a confirmation is a misplaced gesture"
        );
    }

    /// The two dialogs keep separate choices, and neither shows the other's.
    #[test]
    fn a_chosen_archive_fills_only_the_field_that_asked_for_it() {
        let ui = test_window();

        set_server_pack(
            &ui,
            ServerPackTarget::NewServer,
            Some(Path::new("/tmp/one_server_pack.zip")),
        );
        set_server_pack(
            &ui,
            ServerPackTarget::Import,
            Some(Path::new("/tmp/two_server_pack.zip")),
        );

        assert_eq!(ui.get_create_server_pack(), "/tmp/one_server_pack.zip");
        assert_eq!(ui.get_create_server_pack_name(), "one_server_pack.zip");
        assert_eq!(ui.get_import_server_pack(), "/tmp/two_server_pack.zip");
        assert_eq!(ui.get_import_server_pack_name(), "two_server_pack.zip");

        set_server_pack(&ui, ServerPackTarget::Import, None);
        assert_eq!(ui.get_import_server_pack(), "");
        assert_eq!(ui.get_import_server_pack_name(), "");
        assert_eq!(
            ui.get_create_server_pack(),
            "/tmp/one_server_pack.zip",
            "clearing one dialog's choice never touches the other's"
        );
    }

    /// Start is offered from four places. Every one of them has to leave the
    /// user watching the output the run is about to produce, which means
    /// selecting the server and choosing both the page and the workspace that
    /// the console is a tab of.
    #[test]
    fn showing_a_servers_console_selects_it_and_opens_the_console_tab() {
        let ui = test_window();

        ui.set_workspace("instance".into());
        ui.set_workspace_tab("Overview".into());
        ui.set_log_query("stale search".into());
        ui.set_log_follow(false);

        show_server_console(&ui, "repro-server".into());

        assert_eq!(ui.get_selected_server_id(), "repro-server");
        assert_eq!(
            ui.get_page(),
            SERVERS_PAGE,
            "the console is a branch of the servers page, and a page the \
             workspace does not belong to renders nothing"
        );
        assert_eq!(ui.get_workspace(), "server");
        assert_eq!(ui.get_workspace_tab(), "Console");
        assert!(
            ui.get_log_follow(),
            "the output being waited for has not been written yet"
        );
        assert_eq!(
            ui.get_log_query(),
            "",
            "a search left over from another log would hide the run being started"
        );
    }

    /// Started from a client's workspace, Back returns to that client.
    #[test]
    fn starting_a_linked_server_keeps_its_clients_workspace_as_the_way_back() {
        let ui = test_window();

        ui.set_page(INSTANCES_PAGE);
        ui.set_workspace("instance".into());
        ui.set_selected_instance_id("repro-client".into());

        show_server_console(&ui, "repro-server".into());

        assert_eq!(ui.get_page(), SERVERS_PAGE);
        assert_eq!(ui.get_server_workspace_return_client(), "repro-client");
    }

    /// Started from the servers list there is no client to go back to, and a
    /// value left over from an earlier visit would send Back to an instance the
    /// user never came from.
    #[test]
    fn starting_a_server_from_the_servers_list_clears_a_stale_way_back() {
        let ui = test_window();

        ui.set_page(SERVERS_PAGE);
        ui.set_workspace(SharedString::default());
        ui.set_server_workspace_return_client("some-other-client".into());

        show_server_console(&ui, "repro-server".into());

        assert_eq!(ui.get_server_workspace_return_client(), "");
    }

    /// Started from inside the server workspace, whatever brought the user
    /// there still holds, including a client they reached it from.
    #[test]
    fn starting_a_server_from_its_own_workspace_keeps_the_way_back() {
        let ui = test_window();

        ui.set_page(SERVERS_PAGE);
        ui.set_workspace("server".into());
        ui.set_server_workspace_return_client("repro-client".into());

        show_server_console(&ui, "repro-server".into());

        assert_eq!(ui.get_server_workspace_return_client(), "repro-client");
    }

    fn activity(id: &str, status_kind: &'static str) -> ActivityData {
        ActivityData {
            item: ActivityPresentation {
                id: id.to_owned(),
                title: "Import pack".to_owned(),
                status: status_kind.to_owned(),
                status_kind,
                detail: String::new(),
                progress: 0.0,
                indeterminate: true,
                cancellable: false,
                failed: false,
            },
            entity: "Pack".to_owned(),
            time: "12:00:00".to_owned(),
        }
    }

    #[test]
    fn operation_updates_replace_the_same_activity_instead_of_fabricating_history() {
        let mut rows = vec![activity("operation-id", "running")];
        upsert_activity(&mut rows, activity("operation-id", "completed"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].item.status_kind, "completed");
    }

    #[test]
    fn sequential_request_ids_are_distinct_and_start_at_one() {
        let counter = Cell::new(0_u64);
        assert_eq!(next_mod_search_request_id(&counter), Some(1));
        assert_eq!(next_mod_search_request_id(&counter), Some(2));
        assert_eq!(next_mod_search_request_id(&counter), Some(3));
    }

    #[test]
    fn the_last_representable_request_id_is_allocated_exactly_once() {
        let counter = Cell::new(u64::MAX - 1);
        assert_eq!(next_mod_search_request_id(&counter), Some(u64::MAX));
        assert_eq!(counter.get(), u64::MAX);
    }

    #[test]
    fn request_id_allocation_at_the_ceiling_fails_closed_rather_than_repeating_it() {
        let counter = Cell::new(u64::MAX);
        // A failed allocation hands nothing out, so the counter is left
        // exactly where it was rather than advancing past the ceiling.
        assert_eq!(next_mod_search_request_id(&counter), None);
        assert_eq!(counter.get(), u64::MAX);
        // Every later call keeps failing the same way. In particular it never
        // wraps back around to an id an earlier, potentially still in-flight
        // request may already hold, and it never repeats `u64::MAX` itself.
        assert_eq!(next_mod_search_request_id(&counter), None);
        assert_eq!(next_mod_search_request_id(&counter), None);
        assert_eq!(counter.get(), u64::MAX);
    }

    #[test]
    fn duplicate_creation_is_rejected_until_the_original_request_finishes() {
        let key = CreationKey::Client {
            name: "Rendering Repro".to_owned(),
            minecraft: "1.20.1".to_owned(),
            loader: "Forge".to_owned(),
            loader_version: "47.3.0".to_owned(),
            persistent: false,
        };
        let mut tracker = CreationTracker::default();
        let request = tracker.accept(key.clone(), pending_creation(&key, false));
        assert!(request.is_some());
        let request = request.unwrap_or_default();

        assert!(
            tracker
                .accept(key.clone(), pending_creation(&key, false))
                .is_none()
        );
        assert_eq!(tracker.pending.len(), 1);

        tracker.finish(&request);
        assert!(
            tracker
                .accept(key.clone(), pending_creation(&key, false))
                .is_some()
        );
    }

    #[test]
    fn distinct_creation_requests_can_run_together() {
        let first = CreationKey::Server {
            name: "First Server".to_owned(),
            client: "client-one".to_owned(),
            minecraft: "1.20.1".to_owned(),
            loader: "Forge".to_owned(),
            loader_version: "47.3.0".to_owned(),
            persistent: false,
            port: None,
            server_pack: String::new(),
        };
        let mut second = first.clone();
        if let CreationKey::Server { name, .. } = &mut second {
            *name = "Second Server".to_owned();
        }
        let mut tracker = CreationTracker::default();

        assert!(
            tracker
                .accept(first.clone(), pending_creation(&first, false))
                .is_some()
        );
        assert!(
            tracker
                .accept(second.clone(), pending_creation(&second, false))
                .is_some()
        );
        assert_eq!(tracker.pending.len(), 2);
    }

    #[test]
    fn linked_server_deduplication_ignores_hidden_standalone_target_fields() {
        let normalize = |minecraft: &str, loader: &str, loader_version: &str| {
            let client = "client-one";
            let linked = !client.is_empty();
            CreationKey::Server {
                name: "Client One Server".to_owned(),
                client: client.to_owned(),
                minecraft: if linked {
                    String::new()
                } else {
                    minecraft.to_owned()
                },
                loader: if linked {
                    String::new()
                } else {
                    loader.to_owned()
                },
                loader_version: if linked {
                    String::new()
                } else {
                    loader_version.to_owned()
                },
                persistent: false,
                port: None,
                server_pack: String::new(),
            }
        };
        let first = normalize("1.20.1", "Forge", "47.3.0");
        let same_request = normalize("26.2", "NeoForge", "21.0.0");
        let mut tracker = CreationTracker::default();

        assert!(
            tracker
                .accept(first.clone(), pending_creation(&first, false))
                .is_some()
        );
        assert!(
            tracker
                .accept(same_request.clone(), pending_creation(&same_request, false))
                .is_none()
        );
    }

    #[test]
    fn in_memory_activity_is_bounded() {
        let mut rows = Vec::new();
        for index in 0..120 {
            upsert_activity(&mut rows, activity(&index.to_string(), "completed"));
        }
        assert_eq!(rows.len(), 100);
    }

    #[test]
    fn row_filtering_is_case_insensitive_and_searches_exact_metadata() {
        let name = SharedString::from("Vanilla Repro");
        let loader = SharedString::from("Minecraft 1.21.1 | Vanilla");
        assert!(row_matches("repro", [&name, &loader]));
        assert!(row_matches("1.21.1", [&name, &loader]));
        assert!(!row_matches("forge", [&name, &loader]));
    }

    #[test]
    fn java_memory_input_requires_digits_and_an_ordered_nonzero_range() {
        assert_eq!(parse_java_memory("512", "4096"), Ok((512, 4096)));
        assert!(parse_java_memory("512 MiB", "4096").is_err());
        assert!(parse_java_memory("0", "4096").is_err());
        assert!(parse_java_memory("4096", "512").is_err());
    }

    #[test]
    fn every_gui_message_id_exists_in_the_fluent_catalog() {
        for source in [
            include_str!("ui.rs"),
            include_str!("controller.rs"),
            include_str!("backend.rs"),
            include_str!("presentation.rs"),
        ] {
            let mut remaining = source;
            while let Some(start) = remaining.find("\"gui-") {
                let candidate = &remaining[start + 1..];
                let Some(end) = candidate.find('"') else {
                    assert_eq!(candidate, "", "unterminated GUI localization id");
                    break;
                };
                let message_id = &candidate[..end];
                if message_id != "gui-" {
                    assert!(has(message_id), "missing GUI localization: {message_id}");
                }
                remaining = &candidate[end + 1..];
            }
        }
    }

    /// Every callback the window declares, in declaration order.
    ///
    /// Scoped to `AppWindow`'s own callbacks by their indentation, so the ones
    /// belonging to reusable components — `clicked`, `toggled`, and the rest —
    /// are left out. Those are wired inside the markup and have no Rust side.
    fn window_callbacks(source: &str) -> Vec<String> {
        // A renamed component leaves nothing to scan, which the caller's count
        // assertion reports rather than this silently finding zero callbacks.
        let window = source
            .find("export component AppWindow inherits Window {")
            .unwrap_or(source.len());
        source[window..]
            .lines()
            .filter_map(|line| line.strip_prefix("        callback "))
            .filter_map(|rest| rest.split('(').next())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn every_callback_the_window_declares_reaches_the_rust_side() {
        // The exception, and the only kind there is: a callback Rust *invokes*
        // rather than handles. Its implementation is in the markup.
        const INVOKED_BY_RUST: &[&str] = &["present-toast"];

        let declared = window_callbacks(include_str!("ui.rs"));
        assert!(
            declared.len() > 50,
            "the parser found {} callbacks, so it is no longer reading the window component",
            declared.len()
        );

        let controller = include_str!("controller.rs");
        let main = include_str!("main.rs");
        for name in declared {
            if INVOKED_BY_RUST.contains(&name.as_str()) {
                continue;
            }
            let handler = format!("on_{}(", name.replace('-', "_"));
            assert!(
                controller.contains(&handler) || main.contains(&handler),
                "`{name}` is declared on the window and nothing handles it, so the control \
                 that invokes it does nothing"
            );
        }
    }

    #[test]
    fn modrinth_search_does_not_reuse_the_server_filter() {
        let source = include_str!("ui.rs");
        assert!(source.contains("text <=> root.mod-query"));
        assert!(source.contains("text <=> root.server-query"));
        assert!(!source.contains("search-mods(root.selected-instance-id, root.server-query)"));
    }

    #[test]
    fn player_count_accepts_positive_digits_only() {
        assert!(validate_player_count("20").is_empty());
        assert!(!validate_player_count("0").is_empty());
        assert!(!validate_player_count("2.5").is_empty());
        assert!(!validate_player_count("many").is_empty());
    }

    #[test]
    fn creation_forms_use_catalog_models_for_exact_versions() {
        let source = include_str!("ui.rs");
        assert!(source.contains("model: root.minecraft-releases"));
        assert!(source.contains("model: root.create-loader-versions"));
        assert!(source.contains("model: root.create-server-loader-versions"));
        assert!(!source.contains("text <=> root.create-minecraft"));
        assert!(!source.contains("text <=> root.create-loader-version"));
        assert!(source.contains("changed create-minecraft"));
        assert!(source.contains("root.warm-language-catalog(root.create-minecraft)"));
    }

    #[test]
    fn account_row_actions_use_the_stable_id_instead_of_presentation_data() {
        let rows = account_rows(vec![AccountData {
            id: "069a79f4-44e9-4726-a5be-fca90e38aaf5".to_owned(),
            name: "MelanX".to_owned(),
            kind: "Microsoft".to_owned(),
            is_default: false,
            credential_state: "Credential stored securely".to_owned(),
        }]);
        assert_eq!(rows[0].id, "069a79f4-44e9-4726-a5be-fca90e38aaf5");
        assert_eq!(rows[0].name, "MelanX");

        let source = include_str!("ui.rs");
        assert!(source.contains("root.set-default-account(account.id)"));
        assert!(source.contains("root.remove-account(account.id)"));
        assert!(!source.contains("root.set-default-account(account.name)"));
        assert!(!source.contains("root.remove-account(account.name)"));
    }

    #[test]
    fn context_menus_expose_entity_actions_without_redundant_inspector_deletion() {
        let source = include_str!("ui.rs");
        for label in [
            "gui-open-workspace",
            "gui-launch",
            "gui-open-folder",
            "gui-create-linked-server",
            "gui-duplicate-instance",
            "gui-delete-instance",
            "gui-open-server-workspace",
            "gui-restart",
            "gui-force-kill",
            "gui-delete-server",
        ] {
            assert!(source.contains(label), "missing context action {label}");
        }
        assert!(source.matches("ContextMenuArea {").count() >= 3);
    }

    fn plan(install_count: usize, targets: usize) -> backend::ModPlanData {
        backend::ModPlanData {
            instance: "instance".to_owned(),
            provider: "modrinth".to_owned(),
            requested: "utilitix".to_owned(),
            project: "utilitix".to_owned(),
            title: "UtilitiX".to_owned(),
            version: "21.1.6".to_owned(),
            version_id: "abcd1234".to_owned(),
            install_count,
            dependency_count: 1,
            optional_count: 0,
            already_installed_count: 0,
            warning_count: 0,
            linked: backend::LinkedSectionData {
                instance: "instance".to_owned(),
                kind: "install".to_owned(),
                path: String::new(),
                version: "abcd1234".to_owned(),
                name: "UtilitiX".to_owned(),
                version_number: "21.1.6".to_owned(),
                support: String::new(),
                targets: (0..targets)
                    .map(|index| backend::LinkedTargetData {
                        id: format!("server-{index}"),
                        name: format!("Server {index}"),
                        detail: String::new(),
                        state: String::new(),
                        explanation: String::new(),
                        tone: String::new(),
                        actionable: true,
                        selected: true,
                    })
                    .collect(),
            },
        }
    }

    /// Recording a favorite must not rewrite the release list.
    ///
    /// A `ComboBox` rebuilds its selection whenever its model changes, so a
    /// reorder under an open dialog moves the user's choice to whatever lands
    /// at the index it was holding. Slint documents that writing the value back
    /// cannot undo it, which is why the list is left alone until it is loaded.
    #[test]
    fn recording_a_favorite_does_not_reorder_the_release_list() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().expect("window");
        let releases = ["1.21.4", "1.21.1", "1.20.1", "1.16.3"];
        ui.set_minecraft_releases(model(releases.map(SharedString::from).to_vec()));
        ui.set_create_minecraft("1.16.3".into());

        apply_favorite_versions(&ui, vec!["1.16.3".to_owned()]);

        assert_eq!(ui.get_create_minecraft(), "1.16.3");
        assert_eq!(
            ui.get_minecraft_releases()
                .iter()
                .collect::<Vec<SharedString>>(),
            releases.map(SharedString::from).to_vec()
        );
        // The star still lights, and the list of favorites is what was saved.
        assert!(ui.get_create_minecraft_favorite());
        assert_eq!(
            ui.get_favorite_minecraft_versions()
                .iter()
                .collect::<Vec<SharedString>>(),
            vec![SharedString::from("1.16.3")]
        );
    }

    #[test]
    fn a_snapshot_that_changes_nothing_leaves_the_list_model_alone() {
        let rows = |names: &[&str]| {
            names
                .iter()
                .map(|name| SharedString::from(*name))
                .collect::<Vec<_>>()
        };
        let list = model(rows(&["a", "b", "c"]));

        // `ModelRc`'s equality is identity, which is the question here: did the
        // repeater keep the model it was already showing?
        let kept = |updated: &ModelRc<SharedString>| updated == &list;

        // The same rows again. Replacing the model here is what used to rebuild
        // every card between the two halves of a double click.
        assert!(
            kept(&reconciled(&list, rows(&["a", "b", "c"]))),
            "an unchanged list must keep the model it already had"
        );

        // A real change is still applied, in place.
        let changed = reconciled(&list, rows(&["a", "z", "c"]));
        assert!(kept(&changed));
        assert_eq!(changed.row_data(1), Some(SharedString::from("z")));

        // Shrinking and growing both stay in the same model.
        let shorter = reconciled(&list, rows(&["a"]));
        assert!(kept(&shorter));
        assert_eq!(shorter.row_count(), 1);
        let longer = reconciled(&list, rows(&["a", "b", "c", "d"]));
        assert!(kept(&longer));
        assert_eq!(longer.row_count(), 4);
        assert_eq!(longer.row_data(3), Some(SharedString::from("d")));
    }

    #[test]
    fn the_browser_installs_without_a_confirmation_when_nothing_is_left_to_decide() {
        assert_eq!(
            plan_disposition(true, &plan(2, 0)),
            PlanDisposition::Install
        );
        // Everything the plan would install is already there, so applying it
        // would say nothing and change nothing.
        assert_eq!(
            plan_disposition(true, &plan(0, 0)),
            PlanDisposition::NothingToDo
        );
        // A linked server is a choice about where the mod goes, and a button
        // press did not answer it.
        assert_eq!(
            plan_disposition(true, &plan(2, 1)),
            PlanDisposition::Confirm
        );
        // A plan reached any other way is always confirmed.
        assert_eq!(
            plan_disposition(false, &plan(2, 0)),
            PlanDisposition::Confirm
        );
    }

    #[test]
    fn the_suggested_name_follows_a_release_that_drops_the_selected_loader() {
        let ui = test_window();

        ui.set_create_minecraft("1.21.1".into());
        ui.set_create_loader("NeoForge".into());
        ui.set_create_loader_version("21.1.249".into());
        recompute_create_name(&ui);
        assert_eq!(ui.get_create_name_suggestion(), "1.21.1-NeoForge-21.1.249");

        // 1.20.1 predates NeoForge's first build, so choosing it resets the
        // loader. The suggestion has to follow, or the dialog offers a name
        // describing a loader it no longer holds.
        ui.set_create_minecraft("1.20.1".into());
        apply_minecraft_selection(&ui, "client", "1.20.1");

        assert_eq!(ui.get_create_loader(), "Vanilla");
        assert_eq!(ui.get_create_loader_version(), "");
        // A vanilla instance is named after its release alone.
        assert_eq!(ui.get_create_name_suggestion(), "1.20.1");
    }

    #[test]
    fn a_name_the_user_typed_is_never_replaced_by_a_suggestion() {
        let ui = test_window();

        ui.set_create_minecraft("1.20.1".into());
        ui.set_create_loader("NeoForge".into());
        ui.set_create_name("Crash repro".into());
        apply_minecraft_selection(&ui, "client", "1.20.1");

        assert_eq!(ui.get_create_loader(), "Vanilla");
        assert_eq!(
            ui.get_create_name(),
            "Crash repro",
            "the suggestion is a placeholder, so it can never overwrite the field"
        );
        assert_eq!(ui.get_create_name_suggestion(), "1.20.1");
    }

    #[test]
    fn the_install_queue_runs_one_request_at_a_time_in_the_order_they_were_pressed() {
        let request = |project: &str| BrowserInstall {
            instance: "repro-client".to_owned(),
            provider: "modrinth".to_owned(),
            project: project.to_owned(),
        };
        let mut queue = BrowserInstallQueue::default();

        // The first press starts at once; the two behind it wait, because two
        // installs into one instance would race each other's recipe write.
        assert_eq!(
            queue.enqueue(request("utilitix")),
            Some(request("utilitix"))
        );
        assert_eq!(queue.enqueue(request("jei")), None);
        assert_eq!(queue.enqueue(request("sodium")), None);
        // Pressing a row that is already waiting means the same single install.
        assert_eq!(queue.enqueue(request("jei")), None);

        assert_eq!(queue.finish(&request("utilitix")), Some(request("jei")));
        assert_eq!(
            queue.finish(&request("sodium")),
            None,
            "not the running one"
        );
        assert_eq!(queue.finish(&request("jei")), Some(request("sodium")));
        assert_eq!(queue.finish(&request("sodium")), None);
        assert_eq!(
            queue.enqueue(request("utilitix")),
            Some(request("utilitix")),
            "an emptied queue starts the next press immediately"
        );
    }

    #[test]
    fn a_queue_reports_every_project_it_still_owes_an_install() {
        let request = |instance: &str, provider: &str, project: &str| BrowserInstall {
            instance: instance.to_owned(),
            provider: provider.to_owned(),
            project: project.to_owned(),
        };
        let mut queue = BrowserInstallQueue::default();
        queue.enqueue(request("repro-client", "modrinth", "utilitix"));
        queue.enqueue(request("repro-client", "modrinth", "jei"));
        queue.enqueue(request("repro-client", "curseforge", "jei"));
        queue.enqueue(request("other-client", "modrinth", "sodium"));

        assert_eq!(
            queue.outstanding("repro-client", "modrinth"),
            ["utilitix", "jei"],
            "the running request counts as outstanding, and another catalog's does not"
        );
        // A closed confirmation ends the request it belonged to, whose identity
        // the closing itself does not carry.
        assert_eq!(
            queue.finish_active(),
            Some(request("repro-client", "modrinth", "jei"))
        );
        assert_eq!(queue.outstanding("repro-client", "modrinth"), ["jei"]);
    }

    #[test]
    fn card_workspace_actions_select_the_target_before_opening_it() {
        let source = include_str!("ui.rs");
        assert!(source.contains(
            "open-workspace(id) => { root.selected-instance-id = id; root.selected-instance-index = index; root.select-instance(id); root.workspace = \"instance\";"
        ));
        assert!(source.contains(
            "open-workspace(id) => { root.selected-global-server-id = id; root.selected-global-server-index = index; root.selected-server-id = id; root.server-workspace-return-client = \"\"; root.select-server(id); root.workspace = \"server\";"
        ));
        // A card's own touch area has to be declared before the layout holding
        // its buttons, so that it sits under them. Slint resolves a pointer
        // event front to back, so a touch area declared after the buttons takes
        // the press first and the button never sees it. What that costs the
        // user is proven in `tests/card_pointer_targets.rs`; this only keeps the
        // arrangement visible where it is written.
        for component in ["component InstanceCard", "component ServerCard"] {
            let start = source.find(component).expect("the card component");
            let end = source[start..]
                .find("\n    component ")
                .map_or(source.len(), |offset| start + offset);
            let card = &source[start..end];
            let area = card
                .find("card-area := TouchArea")
                .expect("a card touch area");
            let layout = card.find("HorizontalLayout {").expect("the card layout");
            assert!(
                area < layout,
                "{component}'s touch area is over its buttons"
            );
        }
    }

    #[test]
    fn minecraft_defaults_have_one_axis_and_never_persist_per_row() {
        let source = include_str!("ui.rs");
        // One axis: the value is the override. There is no second control
        // asking whether the value the user just chose should count.
        assert!(source.contains("checked: root.item.bool-value;"));
        // The management control this replaces must be gone from the row,
        // along with every property that fed it.
        let start = source
            .find("component MinecraftPreferenceRowCard inherits Rectangle")
            .expect("the preference row component");
        let end = source[start..]
            .find("\n    component ")
            .map_or(source.len(), |offset| start + offset);
        let row = &source[start..end];
        assert!(!row.contains("root.item.managed"));
        assert!(!row.contains("can-manage"));
        assert!(!row.contains("manage-hint"));
        assert!(!row.contains("set-managed"));
        assert!(row.contains("root.item.overridden"));

        // One draft, saved as one batch. No row carries its own persistence.
        assert!(source.contains("root.minecraft-save-defaults()"));
        assert!(source.contains("root.minecraft-discard-defaults()"));
        assert!(
            !source.contains("save-preference"),
            "the per-row persistence this screen replaces must be gone"
        );
        assert!(!source.contains("reset-preference"));
    }

    #[test]
    fn activity_details_wrap_in_cards_with_content_driven_height() {
        let source = include_str!("ui.rs");
        assert!(source.contains("component ActivityCard inherits Rectangle"));
        assert!(source.contains("height: max(102px, detail-text.preferred-height + 82px)"));
        assert!(source.contains("detail-text := BodyText { text: item.detail; visible: item.detail != \"\"; wrap: word-wrap; }"));
    }

    #[test]
    fn global_navigation_keeps_diagnostics_contextual() {
        let source = include_str!("ui.rs");
        let logs = "gui-logs";
        assert!(!source.contains(&format!(
            r#"label: GuiI18n.text("{logs}"); selected: root.page == 2"#
        )));
        assert!(source.contains("root.load-diagnostics(\"client\""));
        assert!(source.contains("root.load-diagnostics(\"server\""));
    }

    #[test]
    fn wide_forms_and_server_details_use_hard_readability_bounds() {
        let source = include_str!("ui.rs");
        assert!(source.contains("width: min(parent.width, 980px)"));
        assert!(source.contains("width: min(parent.width, 1100px)"));
        assert!(!source.contains("max-width: 980px"));
        assert!(!source.contains("max-width: 1100px"));
    }

    #[test]
    fn settings_use_scrollable_reusable_preference_rows() {
        let source = include_str!("ui.rs");
        // One reusable row component drives every preference: nearly two
        // hundred hand-written rows would be a second copy of the catalog.
        assert!(source.contains("component MinecraftPreferenceRowCard inherits Rectangle"));
        assert!(
            source
                .contains("for row in root.minecraft-preference-rows : MinecraftPreferenceRowCard")
        );
        assert!(source.contains("ScrollView { viewport-width: self.width;"));
    }

    #[test]
    fn a_finished_minecraft_save_is_the_only_thing_that_releases_the_screen() {
        let backend = include_str!("backend.rs");
        // Both outcomes send the completion, so a refused batch cannot leave
        // the screen disabled waiting for an event that never comes.
        assert!(backend.contains("Event::MinecraftPreferencesSaveFinished { succeeded }"));
        assert!(backend.contains("let succeeded = outcome.is_ok();"));
        assert!(
            !backend.contains("Event::MinecraftPreferencesSaved"),
            "the success-only event it replaces must be gone"
        );

        let controller = include_str!("controller.rs");
        // The flag is mirrored from the model rather than set beside it, and
        // the completion is the only place it is released.
        assert!(controller.contains("ui.set_minecraft_saving(state.is_saving());"));
        assert!(controller.contains("minecraft.borrow_mut().finish_save(succeeded);"));
        let production = controller.split("#[cfg(test)]").next().unwrap_or_default();
        assert_eq!(
            production.matches("set_minecraft_saving(").count(),
            1,
            "the in-flight flag has exactly one writer"
        );
        // A generic error must never be mistaken for this save completing:
        // the flag is released from the completion arm and nowhere else.
        assert!(production.contains("fn refresh_minecraft_totals"));
    }

    #[test]
    fn minecraft_defaults_keep_release_rules_out_of_the_frontend() {
        let source = include_str!("ui.rs");
        let start = source
            .find("component MinecraftPreferenceRowCard inherits Rectangle")
            .expect("the preference row component");
        let end = source[start..]
            .find("\n    component ")
            .map_or(source.len(), |offset| start + offset);
        let row_component = &source[start..end];
        // Which release does what is core's answer, delivered per row as a
        // status. A version compared inside the row would be a second,
        // unreviewable compatibility rule living in the frontend.
        for release in ["1.12.2", "1.13", "1.20.1", "1.21", "26.2"] {
            assert!(
                !row_component.contains(release),
                "the row must not carry a per-version Minecraft rule ({release})"
            );
        }
        assert!(row_component.contains("root.item.status-kind"));
    }

    #[test]
    fn the_window_size_is_picked_with_a_window_rather_than_a_drawing_of_one() {
        let source = include_str!("ui.rs");
        // The picker is an actual window the window manager sizes, so what the
        // user sees on screen is the window they are asking Minecraft for.
        assert!(source.contains("export component WindowSizePreview inherits Window {"));
        assert!(
            !source.contains("WindowSizeCanvas"),
            "the drawn stand-in it replaces must be gone"
        );
        let controller = include_str!("controller.rs");
        let production = controller.split("#[cfg(test)]").next().unwrap_or_default();
        let preview = source
            .split("export component WindowSizePreview inherits Window {")
            .nth(1)
            .and_then(|rest| rest.split("\n    export component ").next())
            .expect("the sizing window's own markup");
        // The smallest window this build accepts is one rule in one place.
        assert!(preview.contains("min-width: root.minimum-edge;"));
        assert!(preview.contains("min-height: root.minimum-edge;"));
        // A held ratio reaches the window manager as an aspect rather than as
        // a size the window refuses, so every edge and corner keeps working
        // and the manager moves the one the user is not holding. Nothing here
        // resizes a window somebody else is dragging.
        assert!(
            !preview.contains("max-height:"),
            "a ratio must not be kept by taking away the window's own handles"
        );
        assert!(production.contains("window_aspect::hold(window.window(), held);"));

        let aspect = include_str!("window_aspect.rs");
        assert!(aspect.contains("hints.aspect = ratio.and_then("));
        // Read before writing: the rest of that property is the toolkit's.
        assert!(aspect.contains("WmSizeHints::get_normal_hints(&connection, identity)"));
        assert!(
            aspect.contains("#[cfg(not(target_os = \"linux\"))]"),
            "a platform without the property still gets a working picker"
        );

        let start = source
            .find("component MinecraftPreferenceRowCard inherits Rectangle")
            .expect("the preference row component");
        let end = source[start..]
            .find("\n    component ")
            .map_or(source.len(), |offset| start + offset);
        let row_component = &source[start..end];
        assert!(row_component.contains("root.open-window-preview(root.item.id)"));
        for drawn in ["preview-across", "preview-down", "can-resize"] {
            assert!(
                !row_component.contains(drawn),
                "the row has no rectangle left to describe ({drawn})"
            );
        }

        assert!(production.contains("window.set_minimum_edge(minimum_window_edge());"));
        assert!(
            production.contains(
                "fn minimum_window_edge() -> f32 {\n    to_edge(WindowSize::MINIMUM_EDGE)"
            )
        );
        // The window reports a size and the model answers with the exact one.
        assert!(
            production.contains("state.borrow_mut().fit_window(&preference, reported, anchor);")
        );
        // A window manager owns the window for as long as the user holds it,
        // and "held still" cannot be told from "let go". Resizing it on that
        // guess puts this build and the pointer on the same window, which
        // climbs away from the user, so a drag is never answered with a size.
        assert!(
            production.contains("settle_window_size(&gesture, &ui_settling, &state, &sender);")
        );
        assert!(production.contains(".start(TimerMode::SingleShot, WINDOW_SETTLE, move || {"));
        let settling = production
            .split("fn settle_window_size(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .unwrap_or_default();
        assert!(
            !settling.contains("put_window_on"),
            "settling a drag must not resize the window the user is holding"
        );
        assert!(
            !production
                .contains("request_minecraft_preview(&ui, &state, &sender);\n        refresh"),
            "the previewed release must not be asked once per pointer movement"
        );
        // One edge for the whole drag, taken from the window's own history.
        assert!(production.contains("window_anchor(previous, reported)"));
        assert!(production.contains("gesture.driving.set(Some(anchor));"));
        // The edited field decides which edge a held ratio keeps, so the axis
        // travels with the commit instead of being guessed downstream.
        assert!(
            production
                .contains("model.commit_window_size(id, &width, &height, WindowAxis::Width);")
        );
        assert!(
            production
                .contains("model.commit_window_size(id, &width, &height, WindowAxis::Height);")
        );
        for owned_by_the_model in ["project_height", "project_width", "projection_of"] {
            assert!(
                !production.contains(owned_by_the_model),
                "the controller must forward the size rather than repeat the model's arithmetic \
                 ({owned_by_the_model})"
            );
        }
        // A resize settles its own row, so the control the pointer is on and
        // the field the user is in both survive it.
        assert!(production.contains("refresh_minecraft_row(&ui, &state, &preference);"));
        assert!(
            production.contains("ui.get_minecraft_preference_rows().set_row_data(index, row);")
        );
        // An orphaned sizing window would hold the event loop open with no way
        // back to the application.
        assert!(production.contains("ui.window().on_close_requested(move || {"));
    }

    /// The selection is addressed in rows and character columns and stored in
    /// byte offsets, and the two only agree while a line stays ASCII.
    #[test]
    fn a_selection_across_rows_copies_exactly_what_it_covers() {
        let mut state = LiveViewerState::default();
        state.chunks.set_vec(vec![
            chunk("first line"),
            chunk("second line"),
            chunk("third line"),
        ]);

        // Drag from the middle of the first row into the middle of the third.
        state.select_at(0, 6, false);
        state.select_at(2, 5, true);

        assert_eq!(
            state.selected_text().as_deref(),
            Some("line\nsecond line\nthird"),
            "both ends are trimmed to their own column, everything between is whole"
        );
    }

    /// A drag upwards makes the anchor the later of the two ends.
    #[test]
    fn a_selection_made_backwards_reads_the_same_as_one_made_forwards() {
        let mut state = LiveViewerState::default();
        state
            .chunks
            .set_vec(vec![chunk("alpha"), chunk("beta"), chunk("gamma")]);

        state.select_at(2, 3, false);
        state.select_at(0, 2, true);

        assert_eq!(state.selected_text().as_deref(), Some("pha\nbeta\ngam"));
        let (start, end) = state.selection.expect("a selection").ordered();
        assert_eq!((start.row, end.row), (0, 2));
    }

    /// A column is a count of characters; an offset is a count of bytes.
    #[test]
    fn a_column_past_a_multibyte_character_becomes_the_right_byte_offset() {
        let mut state = LiveViewerState::default();
        state.chunks.set_vec(vec![chunk("héllo wörld")]);

        state.select_at(0, 0, false);
        state.select_at(0, 5, true);

        assert_eq!(
            state.selected_text().as_deref(),
            Some("héllo"),
            "five characters, six bytes"
        );
    }

    /// Dragging past the end of a short line selects to its end rather than
    /// nothing, which is what a ragged block of text does constantly.
    #[test]
    fn a_column_past_the_end_of_a_line_is_the_end_of_that_line() {
        let mut state = LiveViewerState::default();
        state
            .chunks
            .set_vec(vec![chunk("short"), chunk("a much longer line")]);

        state.select_at(0, 900, false);
        assert_eq!(
            state.selection.expect("a selection").anchor.offset,
            "short".len()
        );

        state.select_at(1, 6, true);
        assert_eq!(state.selected_text().as_deref(), Some("\na much"));
    }

    #[test]
    fn selecting_everything_covers_every_row_shown() {
        let mut state = LiveViewerState::default();
        state
            .chunks
            .set_vec(vec![chunk("one"), chunk("two"), chunk("three")]);

        state.select_all();

        assert_eq!(state.selected_text().as_deref(), Some("one\ntwo\nthree"));
    }

    #[test]
    fn a_press_with_no_drag_selects_nothing_to_copy() {
        let mut state = LiveViewerState::default();
        state.chunks.set_vec(vec![chunk("one")]);

        state.select_at(0, 2, false);

        assert!(state.selection.expect("a point").is_empty());
        assert_eq!(state.selected_text(), None);
    }

    /// A record can be a whole stack trace, and the row arithmetic a drag uses
    /// is only true if every row is one line.
    #[test]
    fn a_multiline_record_becomes_one_row_for_each_of_its_lines() {
        let mut chunks = Vec::new();
        push_record_chunks(
            &mut chunks,
            "java.lang.Exception: boom\n\tat Foo.bar(Foo.java:1)\n\tat Foo.baz(Foo.java:2)",
            LogLevel::Error,
            32 * 1024,
        );

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[1].text, "\tat Foo.bar(Foo.java:1)");
        assert!(
            chunks.iter().all(|chunk| !chunk.text.contains('\n')),
            "a row holding two lines would be two rows tall and break the arithmetic"
        );
    }

    /// A log can hold a line with no newline in it at all, which is what the
    /// byte cap exists for.
    #[test]
    fn one_enormous_line_is_still_split_into_bounded_rows() {
        let mut chunks = Vec::new();
        push_record_chunks(&mut chunks, &"x".repeat(2_500), LogLevel::Info, 1_000);

        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|chunk| chunk.text.len() <= 1_000));
        assert_eq!(
            chunks.iter().map(|chunk| chunk.text.len()).sum::<usize>(),
            2_500,
            "splitting a line must not lose any of it"
        );
    }

    fn chunk(text: &str) -> LogChunk {
        LogChunk {
            text: text.into(),
            level: "info".into(),
        }
    }

    /// The thing a log is read for is an id, a version or a file name, and a
    /// double click has to give one of those whole.
    #[test]
    fn a_double_click_takes_the_whole_identifier_under_it() {
        let line = "instance 6d000c93-8762-4090-89d8-097277cd6237 started";
        let uuid = line.find("6d00").expect("the id");
        let (start, end) = word_bounds(line, uuid + 4).expect("a word");
        assert_eq!(&line[start..end], "6d000c93-8762-4090-89d8-097277cd6237");

        let jar = "mods/ftb-library-neoforge-26.1.2.7.jar";
        let inside = jar.find("library").expect("inside the name");
        let (start, end) = word_bounds(jar, inside).expect("a word");
        assert_eq!(
            &jar[start..end],
            "ftb-library-neoforge-26.1.2.7.jar",
            "a path separator bounds the word, so a click gives one segment"
        );
    }

    /// A click on something that is not a word takes exactly that character,
    /// which is small and predictable rather than nothing at all.
    #[test]
    fn a_double_click_off_a_word_takes_one_character() {
        let line = "LibX 26.1.2 (libx)";
        let space = line.find(' ').expect("a space");
        assert_eq!(word_bounds(line, space), Some((space, space + 1)));

        let bracket = line.find('(').expect("a bracket");
        assert_eq!(word_bounds(line, bracket), Some((bracket, bracket + 1)));
    }

    #[test]
    fn a_double_click_past_the_end_of_a_line_still_finds_its_last_word() {
        let line = "NeoForge 26.1.2.101";
        let (start, end) = word_bounds(line, line.len() + 40).expect("a word");
        assert_eq!(&line[start..end], "26.1.2.101");

        assert_eq!(word_bounds("", 0), None, "an empty row has no word");
    }

    /// Offsets are bytes, and a log is not always ASCII.
    #[test]
    fn a_word_containing_multibyte_characters_is_taken_whole() {
        let line = "mod Ünïcödé-1.0 loaded";
        let inside = line.find("nïc").expect("inside the word");
        let (start, end) = word_bounds(line, inside).expect("a word");
        assert_eq!(&line[start..end], "Ünïcödé-1.0");
    }

    #[test]
    fn double_clicking_a_row_selects_that_words_text() {
        let mut state = LiveViewerState::default();
        state.chunks.set_vec(vec![chunk("start 26.1.2.101 end")]);

        // The column of the "1" in the version, counted in characters.
        state.select_word_at(0, 8);

        assert_eq!(state.selected_text().as_deref(), Some("26.1.2.101"));
    }

    /// On X11 the clipboard's owner is the context, and whoever pastes asks it
    /// for the bytes. A context built per copy is gone before anyone can ask,
    /// which does not merely fail to copy -- it empties the clipboard.
    ///
    /// Guarded against the source because the failure needs a display to
    /// observe, and by the time anyone notices they have already lost whatever
    /// they had copied before.
    #[test]
    fn the_clipboard_owner_outlives_the_copy_that_wrote_it() {
        let source = include_str!("controller.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("the module above its tests");

        assert!(production.contains("static CLIPBOARD: RefCell<Option<ClipboardContext>>"));
        assert_eq!(
            production.matches("ClipboardContext::new()").count(),
            1,
            "one context is built, and it is the long-lived one"
        );
        let copy = production
            .split("fn copy_to_clipboard(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .unwrap_or_default();
        assert!(
            copy.contains("CLIPBOARD.with("),
            "a copy goes through the context the window owns"
        );
    }

    /// A tab has no glyph, so a console that passes one through draws the
    /// missing-character box. FML's mod list is two tabs deep on every line.
    #[test]
    fn tabs_become_the_spaces_they_stand_for() {
        assert_eq!(expand_tabs("\t\tFTB Library"), "        FTB Library");
        assert_eq!(
            expand_tabs("no tabs here"),
            "no tabs here",
            "text without one is returned untouched"
        );
    }

    /// A tab stop is counted from the start of its own line, so how far one
    /// advances depends on what precedes it, and a record can hold many lines.
    #[test]
    fn a_tab_advances_to_the_next_stop_on_its_own_line() {
        assert_eq!(
            expand_tabs("ab\tc"),
            "ab  c",
            "two columns to the next stop"
        );
        assert_eq!(expand_tabs("abc\td"), "abc d", "one column");
        assert_eq!(expand_tabs("abcd\te"), "abcd    e", "a full stop");
        assert_eq!(
            expand_tabs("abcd\ne\tf"),
            "abcd\ne   f",
            "the second line counts from its own start, not from the first"
        );
    }

    /// The console's selection maps a pointer position to a column by dividing
    /// by one character's width, which is only true while every character is
    /// one column wide.
    #[test]
    fn an_expanded_row_is_as_wide_as_it_looks() {
        let mut chunks = Vec::new();
        push_record_chunks(
            &mut chunks,
            &expand_tabs("\t\tLibX"),
            LogLevel::Info,
            32 * 1024,
        );

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text.chars().count(), 12);
        assert!(
            !chunks[0].text.contains('\t'),
            "a tab left in a row would make its columns lie"
        );
    }

    /// A plain document keeps the native selection; the console cannot.
    ///
    /// A text item's selection is confined to the element it belongs to, so on
    /// a console built from one element per line it can never cross a line.
    /// The console therefore drives the selection itself, which is what the
    /// per-row pointer area and the offsets applied to each row are for. A
    /// crash report is one element and keeps the native behavior.
    #[test]
    fn a_document_selects_natively_and_the_console_selects_across_rows() {
        let source = include_str!("ui.rs");
        assert!(source.contains("accessible-read-only: true"));
        assert!(source.contains("read-only: true"));
        assert!(source.contains("selectable-input.copy()"));
        assert!(source.contains("selectable-input.select-all()"));
        assert!(source.contains(
            "SelectableText { text: root.diagnostic-content; font-size: root.technical-text-size * 1px; }"
        ));

        assert!(source.contains("component LiveLogViewer inherits Rectangle"));
        assert!(source.contains("root.copy-live-log()"));
        // The row's own pointer area, declared after the text item so it takes
        // the press before a single-element selection can begin.
        assert!(source.contains("select := TouchArea"));
        assert!(
            !source.contains("chunk-input.copy()") && !source.contains("chunk-input.select-all()"),
            "a per-row copy would silently copy one line of a selection that spans many"
        );
        // The selection is applied to every row, including one built later by
        // the virtualized list.
        assert!(source.contains("chunk-input.set-selection-offsets("));
        assert!(source.contains("init => { self.apply-selection(); }"));
        // Colors are per level and untouched by any of it.
        assert!(source.contains(r#"chunk.level == "error" || chunk.level == "fatal""#));
    }

    #[test]
    fn a_sources_whole_content_is_drawn_even_when_the_view_is_not_following() {
        let source = LiveLogSource::Client(ephervex_model::InstanceId::new());
        let mut state = LiveViewerState::default();
        state.activate(source.clone());
        let line = |text: &str| {
            LiveLogLine::Client(ephervex_core::live_diagnostics::TailedLine {
                text: text.to_owned(),
                truncation: None,
            })
        };

        // A saved log is opened with following off, so an arrival that is only
        // counted rather than drawn leaves the view empty until it is opened a
        // second time.
        assert_eq!(
            state.apply(LiveLogUpdate {
                source: source.clone(),
                reset: true,
                missing: false,
                source_truncated: false,
                lines: vec![line("[12:00:00] [main/INFO]: opened")],
            }),
            LiveLogChange::Reset
        );
        // Growth is the case the pending-lines count exists for.
        assert_eq!(
            state.apply(LiveLogUpdate {
                source: source.clone(),
                reset: false,
                missing: false,
                source_truncated: false,
                lines: vec![line("[12:00:01] [main/INFO]: grew")],
            }),
            LiveLogChange::Grew
        );
        // An update for a source this view is not on changes nothing at all.
        assert_eq!(
            state.apply(LiveLogUpdate {
                source: LiveLogSource::Client(ephervex_model::InstanceId::new()),
                reset: true,
                missing: false,
                source_truncated: false,
                lines: vec![line("[12:00:02] [main/INFO]: elsewhere")],
            }),
            LiveLogChange::Nothing
        );
        assert_eq!(state.history.len(), 2);
    }

    #[test]
    fn large_live_history_uses_bounded_production_filter_and_chunk_updates() {
        let source = LiveLogSource::Client(ephervex_model::InstanceId::new());
        let mut state = LiveViewerState::default();
        state.activate(source.clone());

        for batch in 0..100 {
            let lines = (0..100)
                .map(|line| {
                    let index = batch * 100 + line;
                    let level = if index % 25 == 0 { "ERROR" } else { "INFO" };
                    LiveLogLine::Client(ephervex_core::live_diagnostics::TailedLine {
                        text: format!(
                            "[16:{:02}:{:02}] [Server thread/{level}] [large_fixture/]: event {index:05}",
                            (index / 60) % 60,
                            index % 60
                        ),
                        truncation: None,
                    })
                })
                .collect();
            assert_ne!(
                state.apply(LiveLogUpdate {
                    source: source.clone(),
                    reset: batch == 0,
                    missing: false,
                    source_truncated: false,
                    lines,
                }),
                LiveLogChange::Nothing
            );
            let presentation = build_log_presentation(&state);
            sync_log_chunks(&state.chunks, presentation.chunks);
        }

        assert_eq!(state.history.len(), 4_000);
        assert!(state.history.history_truncated());
        assert_eq!(state.chunks.row_count(), 4_000);

        state.filter.set_all(false);
        state.filter.set(LogLevel::Error, true);
        let errors = build_log_presentation(&state);
        assert_eq!(errors.match_count, 160);
        assert!(
            errors
                .chunks
                .iter()
                .all(|chunk| chunk.level == "error" && chunk.text.contains("/ERROR]"))
        );
        sync_log_chunks(&state.chunks, errors.chunks);
        assert_eq!(state.chunks.row_count(), 160);

        state.search_query = "event 09999".to_owned();
        let search = build_log_presentation(&state);
        assert_eq!(search.match_count, 1);
        assert!(search.chunks[0].text.contains("event 09999"));
    }

    #[test]
    fn application_display_settings_use_shared_typography_and_separate_technical_text() {
        let source = include_str!("ui.rs");
        assert!(source.contains("in-out property <float> text-scale: 1"));
        assert!(source.contains("default-font-size: Theme.body-size"));
        assert!(source.contains("callback save-display-settings(int, int)"));
        assert!(source.contains("root.technical-text-size * 1px"));
        assert!(!source.contains("They do not change Minecraft's GUI scale"));
    }

    #[test]
    fn global_server_visibility_defaults_to_standalone_and_can_include_linked_rows() {
        let standalone = ServerRow {
            id: "standalone".into(),
            ..ServerRow::default()
        };
        let linked = ServerRow {
            id: "linked".into(),
            linked_client_id: "client".into(),
            ..ServerRow::default()
        };
        assert!(server_is_globally_visible(&standalone, false));
        assert!(!server_is_globally_visible(&linked, false));
        assert!(server_is_globally_visible(&linked, true));
        assert_eq!(
            global_selection_index(std::slice::from_ref(&standalone), "linked"),
            None,
            "a hidden selected linked server must not retain a stale index"
        );
    }

    #[test]
    fn linked_server_workspace_and_pending_creation_remain_contextual() {
        let source = include_str!("ui.rs");
        assert!(source.contains("for server in root.linked-servers"));
        assert!(source.contains("root.selected-server-id = server.id"));
        assert!(source.contains("pending.linked-client-id == root.selected-instance-id"));
        assert!(source.contains("root.server-workspace-return-client"));
        assert!(source.contains("name + \" Server\""));
    }

    #[test]
    fn provider_and_storage_resets_use_explicit_application_callbacks() {
        let source = include_str!("ui.rs");
        let backend = include_str!("backend.rs");
        assert!(source.contains("callback reset-microsoft-client-id()"));
        assert!(source.contains("callback reset-user-agent()"));
        assert!(source.contains("callback choose-storage-root()"));
        assert!(source.contains("callback reset-storage-root()"));
        assert!(source.contains("root.effective-microsoft-client-id"));
        assert!(source.contains("root.effective-user-agent"));
        assert!(source.contains("root.default-persistent-root"));
        assert!(backend.contains(".pick_folder()"));
        assert!(backend.contains("Event::StorageRootPicked(path)"));
    }

    #[test]
    fn the_preferred_catalog_row_shows_the_stored_value_and_writes_through_core() {
        let source = include_str!("ui.rs");
        let backend = include_str!("backend.rs");
        // One callback into Rust, which is the only way the setting changes:
        // the view has no path to the settings file of its own.
        assert!(source.contains("callback choose-default-mod-catalog(string)"));
        assert!(backend.contains("Command::SaveDefaultModCatalog"));
        assert!(backend.contains("app.set_default_mod_catalog(catalog)"));
        // The chips follow the stored value, so a refused write cannot leave a
        // catalog selected that was never recorded.
        assert!(source.contains("selected: root.default-mod-catalog == \"modrinth\""));
        assert!(source.contains("selected: root.default-mod-catalog == \"curseforge\""));
        // A name a newer Ephervex recorded is reported as itself.
        assert!(source.contains("root.configured-mod-catalog"));
        assert!(source.contains("gui-preferred-mod-catalog-unrecognized"));
        // It lives in the settings surface that already exists.
        assert!(source.contains("root.workspace-tab == \"Providers & APIs\""));
    }

    #[test]
    fn the_preferred_catalog_is_applied_when_an_instance_workspace_opens() {
        let source = include_str!("controller.rs");
        // The lifecycle point the preference applies at, and the same reset a
        // chip click uses, so an in-flight search cannot survive the move.
        assert!(source.contains("presentation::catalog_to_open_on("));
        assert!(source.contains("fn show_mod_catalog(ui: &AppWindow, provider: &str)"));
        // Applying a settings snapshot writes the three settings properties and
        // never the browser's own catalog: saving an unrelated setting must not
        // move a browser the user is in the middle of reading.
        // The whole signature, so a neighbouring reader whose name merely
        // starts the same way cannot be inspected in its place.
        let applied = source
            .split("fn apply_snapshot(")
            .nth(1)
            .unwrap_or_default();
        assert!(applied.contains("ui.set_default_mod_catalog("));
        assert!(!applied.contains("ui.set_mod_provider("));
    }

    #[test]
    fn the_curseforge_credential_row_reports_presence_and_offers_set_replace_and_remove() {
        let source = include_str!("ui.rs");
        // Presence is the only thing the view is told, and the three operations
        // are separate callbacks so removing cannot be expressed as saving an
        // empty value.
        assert!(source.contains("in-out property <bool> curseforge-key-configured"));
        assert!(source.contains("callback save-curseforge-api-key(string)"));
        assert!(source.contains("callback remove-curseforge-api-key()"));
        assert!(source.contains("callback open-curseforge-key-help()"));
        assert!(source.contains("gui-curseforge-key-configured"));
        assert!(source.contains("gui-curseforge-key-not-configured"));
        // Replacing asks for the new value rather than revealing the old one.
        assert!(source.contains("gui-curseforge-key-replace"));
        assert!(source.contains("input-type: InputType.password"));
    }

    #[test]
    fn no_stored_curseforge_key_is_ever_written_back_into_the_view() {
        // The one invariant this surface exists to keep. `apply_settings` may
        // set the *configured* flag and nothing else, and the only writes to
        // the input field anywhere in the controller empty it.
        let source = include_str!("controller.rs");
        // Split so this assertion does not match its own text.
        let write = concat!("ui.set_", "curseforge_api_key(");
        let mut found = 0;
        for (index, _) in source.match_indices(write) {
            found += 1;
            let argument = &source[index + write.len()..];
            assert!(
                argument.starts_with("SharedString::new()"),
                "the input field is written with something other than an empty value"
            );
        }
        assert!(found > 0, "the field is cleared somewhere");
        assert!(source.contains(concat!(
            "ui.set_",
            "curseforge_key_configured(settings.curseforge_key_present)"
        )));
    }

    #[test]
    fn the_key_application_action_points_at_the_official_support_article() {
        let backend = include_str!("backend.rs");
        assert!(backend.contains("open_in_browser(ephervex_core::CURSEFORGE_API_KEY_HELP)"));
        assert!(
            ephervex_core::CURSEFORGE_API_KEY_HELP.starts_with("https://support.curseforge.com/")
        );
    }

    /// Where the Custom entry sits, found the way the view finds it: by asking
    /// for a budget that is not one of the presets.
    fn custom_selection_index() -> usize {
        presentation::cache_limit_selection(ArtifactCacheLimit::Limited(3 * 1024 * 1024 * 1024))
    }

    #[test]
    fn the_cache_limit_selector_offers_every_preset_plus_custom_and_unlimited() {
        let choices = presentation::cache_limit_choices();
        let labels: Vec<&str> = choices.iter().map(|choice| choice.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "2 GiB",
                "5 GiB",
                "10 GiB",
                "25 GiB",
                "50 GiB",
                "100 GiB",
                "Custom",
                "Unlimited",
            ]
        );
    }

    #[test]
    fn the_selector_opens_on_the_stored_budget_including_a_custom_one() {
        let choices = presentation::cache_limit_choices();
        let default = presentation::cache_limit_selection(ArtifactCacheLimit::default());
        assert_eq!(choices[default].label, "10 GiB");

        let unlimited = presentation::cache_limit_selection(ArtifactCacheLimit::Unlimited);
        assert_eq!(
            choices[unlimited].kind,
            presentation::CacheLimitKind::Unlimited
        );

        // 500 GiB is not a preset, so the selector opens on Custom with the
        // fields the user typed rather than snapping to 100 GiB.
        let custom = ArtifactCacheLimit::Limited(500 * 1024 * 1024 * 1024);
        assert_eq!(
            choices[presentation::cache_limit_selection(custom)].kind,
            presentation::CacheLimitKind::Custom
        );
        assert_eq!(
            presentation::cache_limit_custom_fields(custom),
            ("500".to_owned(), "GiB".to_owned())
        );
        assert_eq!(
            presentation::cache_limit_custom_fields(ArtifactCacheLimit::Limited(
                4 * 1024 * 1024 * 1024 * 1024
            )),
            ("4".to_owned(), "TiB".to_owned()),
            "the largest unit that divides exactly is the one shown"
        );
    }

    #[test]
    fn a_custom_size_above_the_last_preset_resolves_to_the_exact_byte_count() {
        let custom = custom_selection_index();

        for (typed, unit, expected) in [
            ("500", "GiB", 500 * 1024 * 1024 * 1024_u64),
            ("1", "TiB", 1024 * 1024 * 1024 * 1024),
            ("4096", "GiB", 4096 * 1024 * 1024 * 1024),
        ] {
            assert_eq!(
                presentation::cache_limit_from_selection(custom, typed, unit),
                Ok(ArtifactCacheLimit::Limited(expected)),
                "{typed} {unit}"
            );
        }
    }

    #[test]
    fn a_custom_size_that_cannot_be_stored_is_refused_rather_than_clamped() {
        let custom = custom_selection_index();

        for (typed, unit) in [
            ("8388608", "TiB"),
            ("18446744073709551615", "TiB"),
            ("", "GiB"),
            ("ten", "GiB"),
            ("-5", "GiB"),
            ("500", "GB"),
        ] {
            assert!(
                presentation::cache_limit_from_selection(custom, typed, unit).is_err(),
                "`{typed} {unit}` should be refused"
            );
        }
    }

    #[test]
    fn a_preset_selection_resolves_without_reading_the_custom_fields() {
        for (index, expected) in [(0_usize, 2), (2, 10), (5, 100)] {
            assert_eq!(
                presentation::cache_limit_from_selection(index, "nonsense", "not a unit"),
                Ok(ArtifactCacheLimit::Limited(expected * 1024 * 1024 * 1024))
            );
        }
        let unlimited = presentation::cache_limit_choices().len() - 1;
        assert_eq!(
            presentation::cache_limit_from_selection(unlimited, "nonsense", "not a unit"),
            Ok(ArtifactCacheLimit::Unlimited)
        );
    }

    #[test]
    fn the_cache_section_measures_on_demand_and_guards_a_second_clear() {
        let source = include_str!("ui.rs");
        // Measuring walks the store, so it must not be on the ordinary refresh
        // path — only on opening the page that shows the figure.
        assert!(source.contains("root.measure-artifact-cache()"));
        assert!(source.contains("callback clear-artifact-cache()"));
        assert!(source.contains("root.artifact-cache-clearing = true"));
        assert!(source.contains("enabled: !root.artifact-cache-clearing"));
        // The size box only exists on the Custom entry, and which entry that is
        // comes from the same list the backend built.
        assert!(
            source.contains("root.artifact-cache-limit-index == root.artifact-cache-custom-index")
        );
        assert!(source.contains("root.artifact-cache-units"));

        let backend = include_str!("backend.rs");
        assert!(
            !backend.contains("artifact_cache_status()) => Event::Snapshot"),
            "the usage figure is not part of the ordinary snapshot"
        );
        // Both events that can end a clear release the guard the button set, so
        // a clear that reports only a failure cannot leave it disabled.
        assert!(backend.contains("Event::ArtifactCacheClearFinished"));
    }

    fn failure(message: &str) -> ImportFailure {
        ImportFailure {
            message: message.to_owned(),
            kind: ImportErrorKind::Other,
        }
    }

    fn resolved_presentation(name: &str, needs_resolution: bool) -> ImportPresentation {
        ImportPresentation {
            format: "CurseForge".to_owned(),
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            author: "An Author".to_owned(),
            minecraft_version: "1.21.1".to_owned(),
            loader: "NeoForge 21.1.42".to_owned(),
            required_count: 5,
            optional_count: 2,
            optional: if needs_resolution {
                Vec::new()
            } else {
                vec![
                    presentation::ImportOptionPresentation {
                        logical_path: "mods/optional-a.jar".to_owned(),
                        display: "mods/optional-a.jar".to_owned(),
                        selected: false,
                    },
                    presentation::ImportOptionPresentation {
                        logical_path: "mods/optional-b.jar".to_owned(),
                        display: "mods/optional-b.jar".to_owned(),
                        selected: true,
                    },
                ]
            },
            needs_provider_resolution: needs_resolution,
            blocked: Vec::new(),
            duplicate_files: Vec::new(),
        }
    }

    fn sample_identity() -> ProviderFileIdentity {
        ProviderFileIdentity {
            provider: Some(ModProvider::CurseForge),
            project_id: Some("238222".to_owned()),
            version_id: Some("4593548".to_owned()),
            file_id: Some("4593548".to_owned()),
        }
    }

    fn blocked_presentation() -> ImportPresentation {
        let mut presentation = resolved_presentation("Example Pack", false);
        presentation.blocked = vec![presentation::BlockedFilePresentation {
            identity: sample_identity(),
            project_line: "Blocked Mod".to_owned(),
            file_line: "blocked.jar".to_owned(),
            browser_url: "https://www.curseforge.com/minecraft/mc-mods/blocked-mod".to_owned(),
        }];
        presentation
    }

    #[test]
    fn a_blocked_file_keeps_the_flow_out_of_ready_until_it_is_skipped() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.analyzed(
            generation,
            PathBuf::from("pack.zip"),
            Ok(blocked_presentation()),
        );

        assert!(
            !flow.is_ready(),
            "a pack with an unresolved blocked file must not be ready to import"
        );
        assert_eq!(flow.blocked.len(), 1);

        flow.skip_blocked_file(&sample_identity());
        assert!(flow.blocked.is_empty());
        assert_eq!(flow.skipped, vec![sample_identity()]);
        assert!(
            flow.is_ready(),
            "skipping the only blocked file clears the way to import"
        );
    }

    #[test]
    fn a_watcher_match_clears_the_row_without_recording_a_skip() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.analyzed(
            generation,
            PathBuf::from("pack.zip"),
            Ok(blocked_presentation()),
        );

        assert!(flow.blocked_file_matched(generation, &sample_identity()));
        assert!(flow.blocked.is_empty());
        assert!(
            flow.skipped.is_empty(),
            "a watcher match is not a skip decision"
        );
        assert!(flow.is_ready());
    }

    #[test]
    fn a_stale_watcher_match_is_discarded() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.analyzed(
            generation,
            PathBuf::from("pack.zip"),
            Ok(blocked_presentation()),
        );

        assert!(
            !flow.blocked_file_matched(generation.wrapping_add(1), &sample_identity()),
            "a match for a superseded attempt must not touch this one"
        );
        assert_eq!(flow.blocked.len(), 1);
    }

    #[test]
    fn a_skip_survives_a_fresh_resolution_result_for_the_same_archive() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.analyzed(
            generation,
            PathBuf::from("pack.zip"),
            Ok(blocked_presentation()),
        );
        flow.skip_blocked_file(&sample_identity());
        assert!(flow.is_ready());

        // A second resolution answer still names the same blocked file (the
        // server has not forgotten it either); the earlier skip must keep it
        // out of the modal rather than reintroducing it.
        flow.resolved(generation, Ok(blocked_presentation()));
        assert!(
            flow.blocked.is_empty(),
            "an already-skipped file must not reappear"
        );
        assert!(flow.is_ready());
    }

    #[test]
    fn begin_submit_carries_the_skipped_identities() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.analyzed(
            generation,
            PathBuf::from("pack.zip"),
            Ok(blocked_presentation()),
        );
        flow.skip_blocked_file(&sample_identity());

        let submission = flow.begin_submit().expect("ready once skipped");
        assert_eq!(submission.skip_blocked, vec![sample_identity()]);
    }

    #[test]
    fn beginning_an_attempt_enters_analyzing_immediately() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        assert_eq!(flow.phase, ImportPhase::Analyzing);
        assert_eq!(flow.tracked_generation, Some(generation));
        assert!(!flow.is_ready());
    }

    #[test]
    fn a_canceled_picker_returns_the_current_attempt_to_idle() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        assert!(flow.pick_canceled(generation));
        assert_eq!(flow.phase, ImportPhase::Idle);
        assert_eq!(flow.tracked_generation, None);
    }

    #[test]
    fn a_stale_cancellation_does_not_touch_a_newer_attempt() {
        let mut flow = ImportFlowState::default();
        let first = flow.begin_attempt();
        let second = flow.begin_attempt();
        assert_ne!(first, second);

        assert!(
            !flow.pick_canceled(first),
            "the first attempt is no longer current"
        );
        assert_eq!(
            flow.phase,
            ImportPhase::Analyzing,
            "the second attempt is untouched"
        );
        assert_eq!(flow.tracked_generation, Some(second));
    }

    #[test]
    fn generations_never_repeat_across_a_reset() {
        let mut flow = ImportFlowState::default();
        let first = flow.begin_attempt();
        flow.reset();
        let second = flow.begin_attempt();
        assert!(
            second > first,
            "a reused number would let a stale result pass the check"
        );
    }

    #[test]
    fn a_local_analysis_result_for_an_abandoned_attempt_is_rejected() {
        let mut flow = ImportFlowState::default();
        let first = flow.begin_attempt();
        let second = flow.begin_attempt();

        let applied = flow.analyzed(
            first,
            PathBuf::from("a.zip"),
            Ok(resolved_presentation("Pack A", false)),
        );
        assert!(!applied, "generation 1 is no longer the current attempt");
        assert_eq!(flow.phase, ImportPhase::Analyzing, "attempt 2 is untouched");
        assert_eq!(flow.tracked_generation, Some(second));
    }

    #[test]
    fn a_stale_local_analysis_failure_does_not_fail_the_current_attempt() {
        let mut flow = ImportFlowState::default();
        let first = flow.begin_attempt();
        let second = flow.begin_attempt();

        let applied = flow.analyzed(first, PathBuf::from("a.zip"), Err(failure("broken")));
        assert!(!applied);
        assert_eq!(flow.phase, ImportPhase::Analyzing, "attempt 2 is untouched");
        assert!(flow.error.is_empty());
        assert_eq!(flow.tracked_generation, Some(second));
    }

    #[test]
    fn closing_the_modal_makes_a_still_arriving_local_analysis_unappliable() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.reset(); // what `on_close_import_modal` does

        let applied = flow.analyzed(
            generation,
            PathBuf::from("a.zip"),
            Ok(resolved_presentation("Pack A", false)),
        );
        assert!(!applied, "the modal was closed before this arrived");
        assert_eq!(flow.phase, ImportPhase::Idle);
    }

    #[test]
    fn a_local_analysis_failure_is_shown_and_blocks_readiness() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        assert!(flow.analyzed(
            generation,
            PathBuf::from("broken.zip"),
            Err(failure("not a pack"))
        ));
        assert_eq!(flow.phase, ImportPhase::Failed);
        assert!(!flow.is_ready());
        assert_eq!(flow.error, "not a pack");
        assert_eq!(flow.error_kind, ImportErrorKind::Other);
    }

    #[test]
    fn a_pack_needing_no_resolution_is_ready_as_soon_as_it_is_analyzed() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        assert!(flow.analyzed(
            generation,
            PathBuf::from("pack.mrpack"),
            Ok(resolved_presentation("Modrinth Pack", false)),
        ));
        assert!(flow.is_ready());
        assert_eq!(flow.options.len(), 2);
    }

    #[test]
    fn a_resolved_analysis_becomes_ready_with_its_optional_defaults_preserved() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.analyzed(
            generation,
            PathBuf::from("pack.zip"),
            Ok(resolved_presentation("Example Pack", true)),
        );
        assert_eq!(flow.phase, ImportPhase::Resolving);
        assert!(!flow.is_ready());

        let applied = flow.resolved(generation, Ok(resolved_presentation("Example Pack", false)));
        assert!(applied);
        assert!(flow.is_ready());
        assert_eq!(flow.options.len(), 2);
        assert!(!flow.options[0].selected, "starts_selected() is preserved");
        assert!(flow.options[1].selected, "starts_selected() is preserved");
    }

    #[test]
    fn a_stale_resolution_result_is_dropped_once_a_newer_archive_is_current() {
        let mut flow = ImportFlowState::default();
        let first = flow.begin_attempt();
        flow.analyzed(
            first,
            PathBuf::from("a.zip"),
            Ok(resolved_presentation("Pack A", true)),
        );
        // A second archive is picked before the first's resolution answers.
        let second = flow.begin_attempt();
        flow.analyzed(
            second,
            PathBuf::from("b.zip"),
            Ok(resolved_presentation("Pack B", false)),
        );
        assert_eq!(flow.phase, ImportPhase::Ready);
        assert_eq!(flow.archive, PathBuf::from("b.zip"));

        // Generation 1's late answer must not touch the now-current attempt.
        let applied = flow.resolved(first, Ok(resolved_presentation("Pack A", false)));
        assert!(!applied);
        assert_eq!(flow.phase, ImportPhase::Ready);
        assert_eq!(flow.archive, PathBuf::from("b.zip"));
        assert_eq!(flow.suggested_name, "Pack B");
    }

    #[test]
    fn a_stale_resolution_failure_does_not_fail_the_current_attempt() {
        let mut flow = ImportFlowState::default();
        let first = flow.begin_attempt();
        flow.analyzed(
            first,
            PathBuf::from("a.zip"),
            Ok(resolved_presentation("Pack A", true)),
        );
        let second = flow.begin_attempt();
        flow.analyzed(
            second,
            PathBuf::from("b.zip"),
            Ok(resolved_presentation("Pack B", true)),
        );

        let applied = flow.resolved(first, Err(failure("stale failure")));
        assert!(!applied);
        assert_eq!(
            flow.phase,
            ImportPhase::Resolving,
            "generation 2 is still resolving"
        );
        assert!(flow.error.is_empty());
    }

    #[test]
    fn a_resolution_failure_makes_the_import_not_ready_until_a_new_archive_is_picked() {
        let mut flow = ImportFlowState::default();
        let first = flow.begin_attempt();
        flow.analyzed(
            first,
            PathBuf::from("pack.zip"),
            Ok(resolved_presentation("Example Pack", true)),
        );

        let applied = flow.resolved(
            first,
            Err(ImportFailure {
                message: "missing CurseForge API key".to_owned(),
                kind: ImportErrorKind::MissingCredential,
            }),
        );
        assert!(applied);
        assert_eq!(flow.phase, ImportPhase::Failed);
        assert!(!flow.is_ready());
        assert!(
            flow.begin_submit().is_none(),
            "a failed analysis must not be submittable"
        );
        assert_eq!(flow.error, "missing CurseForge API key");
        assert_eq!(flow.error_kind, ImportErrorKind::MissingCredential);

        // Choosing another archive recovers normally.
        let second = flow.begin_attempt();
        flow.analyzed(
            second,
            PathBuf::from("other.mrpack"),
            Ok(resolved_presentation("Other Pack", false)),
        );
        assert!(flow.is_ready());
        assert!(flow.error.is_empty());
        assert_eq!(
            flow.error_kind,
            ImportErrorKind::default(),
            "a fresh attempt clears the previous failure's category too"
        );
    }

    #[test]
    fn only_the_first_of_two_submits_is_accepted_and_completion_releases_the_guard() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.analyzed(
            generation,
            PathBuf::from("pack.zip"),
            Ok(resolved_presentation("Example Pack", false)),
        );

        let expected = PreparedImportSubmission {
            generation,
            archive: PathBuf::from("pack.zip"),
            selection: OptionalSelection::of(["mods/optional-b.jar"]),
            skip_blocked: Vec::new(),
        };
        assert_eq!(flow.begin_submit(), Some(expected.clone()));
        assert_eq!(flow.phase, ImportPhase::Submitting);
        assert_eq!(
            flow.begin_submit(),
            None,
            "a second click while the first submission is running must not start another"
        );

        assert!(flow.submit_finished(generation));
        assert!(flow.is_ready(), "a finished submission falls back to Ready");
        assert_eq!(
            flow.begin_submit(),
            Some(expected),
            "the guard is released once the first submission ends"
        );
    }

    #[test]
    fn the_submitted_archive_is_always_the_analyzed_one_regardless_of_anything_else() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.analyzed(
            generation,
            PathBuf::from("/tmp/analyzed-pack.zip"),
            Ok(resolved_presentation("Example Pack", false)),
        );
        flow.mark_name_edited();
        flow.toggle_option("mods/optional-a.jar");

        assert_eq!(
            flow.begin_submit(),
            Some(PreparedImportSubmission {
                generation,
                archive: PathBuf::from("/tmp/analyzed-pack.zip"),
                selection: OptionalSelection::of(["mods/optional-a.jar", "mods/optional-b.jar"]),
                skip_blocked: Vec::new(),
            }),
            "the archive submitted must be exactly the one the current analysis is for; \
             `begin_submit` takes no archive argument, so nothing else could have supplied one"
        );
    }

    #[test]
    fn a_wrong_submission_completion_does_not_release_the_guard() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.analyzed(
            generation,
            PathBuf::from("pack.zip"),
            Ok(resolved_presentation("Example Pack", false)),
        );
        flow.begin_submit();
        assert_eq!(flow.phase, ImportPhase::Submitting);

        assert!(
            !flow.submit_finished(generation.wrapping_add(1)),
            "a completion for a different generation must not release the guard"
        );
        assert_eq!(
            flow.phase,
            ImportPhase::Submitting,
            "an unrelated completion leaves the current submission running"
        );

        assert!(flow.submit_finished(generation));
        assert_eq!(flow.phase, ImportPhase::Ready);
    }

    #[test]
    fn a_submission_completion_for_an_attempt_no_longer_current_is_ignored() {
        let mut flow = ImportFlowState::default();
        let first = flow.begin_attempt();
        flow.analyzed(
            first,
            PathBuf::from("pack.zip"),
            Ok(resolved_presentation("Example Pack", false)),
        );
        flow.begin_submit();

        // The modal is closed while the submission is still running in the
        // background, and a new attempt begins before it finishes.
        flow.reset();
        let second = flow.begin_attempt();

        assert!(
            !flow.submit_finished(first),
            "the old submission's completion must not touch the new attempt"
        );
        assert_eq!(flow.phase, ImportPhase::Analyzing);
        assert_eq!(flow.tracked_generation, Some(second));
    }

    #[test]
    fn submit_finished_is_a_no_op_when_nothing_was_submitting() {
        let mut flow = ImportFlowState::default();
        assert!(!flow.submit_finished(1));
        assert_eq!(flow.phase, ImportPhase::Idle);
    }

    #[test]
    fn a_matching_success_may_close_its_own_still_current_import_modal() {
        assert_eq!(
            import_submission_action("import", true, true),
            ImportSubmissionAction::Close
        );
    }

    #[test]
    fn an_abandoned_generations_success_does_not_close_a_newly_opened_import_modal() {
        // `applied` is what `ImportFlowState::submit_finished` reports for a
        // stale generation: `false`, regardless of `succeeded`, regardless of
        // which modal happens to be open now (even the reopened import one).
        assert_eq!(
            import_submission_action("import", false, true),
            ImportSubmissionAction::Ignore
        );
    }

    #[test]
    fn an_abandoned_generations_success_does_not_close_an_unrelated_modal() {
        assert_eq!(
            import_submission_action("create-server", true, true),
            ImportSubmissionAction::Ignore
        );
        // Both conditions have to hold: even a matching generation must not
        // reach into a modal the user has since navigated to.
        assert_eq!(
            import_submission_action("create-server", false, true),
            ImportSubmissionAction::Ignore
        );
    }

    #[test]
    fn a_stale_submission_completion_cannot_alter_a_newer_flow() {
        let mut flow = ImportFlowState::default();
        let first = flow.begin_attempt();
        flow.analyzed(
            first,
            PathBuf::from("pack.zip"),
            Ok(resolved_presentation("Example Pack", false)),
        );
        flow.begin_submit();
        flow.reset();
        let second = flow.begin_attempt();

        // The old submission's own terminal event still arrives, generation
        // and all, after a newer attempt is already current.
        let applied = flow.submit_finished(first);
        assert!(!applied, "generation 1 no longer names the current attempt");
        assert_eq!(
            import_submission_action("import", applied, true),
            ImportSubmissionAction::Ignore,
            "a stale completion must not act on the modal even if it succeeded"
        );
        assert_eq!(flow.phase, ImportPhase::Analyzing, "attempt 2 is untouched");
        assert_eq!(flow.tracked_generation, Some(second));
    }

    #[test]
    fn a_matching_non_success_releases_the_guard_and_only_re_renders() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.analyzed(
            generation,
            PathBuf::from("pack.zip"),
            Ok(resolved_presentation("Example Pack", false)),
        );
        flow.begin_submit();

        let applied = flow.submit_finished(generation);
        assert!(applied, "the matching generation releases the guard");
        assert!(flow.is_ready(), "failure/cancellation falls back to Ready");
        assert_eq!(
            import_submission_action("import", applied, false),
            ImportSubmissionAction::Render,
            "failure, cancellation, and setup failure all pass succeeded: false, \
             and none of them may close the modal"
        );
    }

    #[test]
    fn selected_optional_logical_paths_become_the_exact_optional_selection() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.analyzed(
            generation,
            PathBuf::from("pack.zip"),
            Ok(resolved_presentation("Example Pack", false)),
        );
        // "mods/optional-a.jar" starts unselected, "mods/optional-b.jar" starts selected.
        flow.toggle_option("mods/optional-a.jar");

        assert_eq!(
            flow.selection(),
            OptionalSelection::of(["mods/optional-a.jar", "mods/optional-b.jar"])
        );
    }

    #[test]
    fn toggling_an_unknown_logical_path_changes_nothing() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.analyzed(
            generation,
            PathBuf::from("pack.zip"),
            Ok(resolved_presentation("Example Pack", false)),
        );
        flow.toggle_option("mods/does-not-exist.jar");
        assert_eq!(
            flow.selection(),
            OptionalSelection::of(["mods/optional-b.jar"])
        );
    }

    #[test]
    fn an_edit_made_while_resolving_survives_the_resolution_finishing() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.analyzed(
            generation,
            PathBuf::from("a.zip"),
            Ok(resolved_presentation("Pack A", true)),
        );
        flow.mark_name_edited();

        // Resolution finishes and reports a name-bearing presentation for the
        // same attempt; the user's edit must still win.
        let applied = flow.resolved(
            generation,
            Ok(resolved_presentation("Pack A (Full)", false)),
        );
        assert!(applied);
        assert_eq!(
            flow.suggested_name, "Pack A",
            "the edited flag must block the overwrite"
        );
        assert!(flow.name_edited);
    }

    #[test]
    fn a_new_archive_deterministically_resets_the_suggested_name_and_the_edited_flag() {
        let mut flow = ImportFlowState::default();
        let first = flow.begin_attempt();
        flow.analyzed(
            first,
            PathBuf::from("a.zip"),
            Ok(resolved_presentation("Pack A", false)),
        );
        flow.mark_name_edited();

        let second = flow.begin_attempt();
        flow.analyzed(
            second,
            PathBuf::from("b.zip"),
            Ok(resolved_presentation("Pack B", false)),
        );
        assert_eq!(
            flow.suggested_name, "Pack B",
            "a new archive always starts a fresh suggestion"
        );
        assert!(!flow.name_edited);
    }

    #[test]
    fn closing_the_modal_resets_the_flow_so_a_stale_resolution_cannot_apply() {
        let mut flow = ImportFlowState::default();
        let generation = flow.begin_attempt();
        flow.analyzed(
            generation,
            PathBuf::from("pack.zip"),
            Ok(resolved_presentation("Example Pack", true)),
        );
        flow.reset();

        let applied = flow.resolved(generation, Ok(resolved_presentation("Example Pack", false)));
        assert!(
            !applied,
            "a resolution for an abandoned attempt must not apply"
        );
        assert_eq!(flow.phase, ImportPhase::Idle);
    }
}
