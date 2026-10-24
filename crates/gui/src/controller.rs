//! Slint callback registration and UI model updates.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Sender, TryRecvError};
use std::time::{Duration, Instant};

use copypasta::{ClipboardContext, ClipboardProvider as _};
use ephervex_core::live_diagnostics::{LogFilter, LogLevel, RetainedLog};
use ephervex_core::{
    InstalledHost, OptionalSelection, ProviderFileIdentity, ServerArchiveEvidence, StorageCategory,
    StorageRetention, WindowSize,
};
use ephervex_i18n::{Arg, lookup, lookup_args};
use ephervex_model::AbsentModPolicy;
use ephervex_model::ModProvider;
use slint::{
    ComponentHandle as _, Image, LogicalSize, Model as _, ModelRc, SharedString, Timer, TimerMode,
    VecModel,
};

use crate::backend::{
    self, AccountData, ActivityData, ArtifactCacheStatusData, Command, DiagnosticContent,
    DiagnosticData, Event, IsolationData, JavaData, LiveLogLine, LiveLogSource, LiveLogUpdate,
    ManagedModData, ModSearchData, ServerPackTarget, ServerWorkspaceData, SettingsData, Snapshot,
};
use crate::binding_capture::{BindingCapture, CapturedInput};
use crate::file_drop::{FileDrop, FileDrops};
use crate::minecraft_defaults::{
    self, LanguageCatalogState, MinecraftDefaultsModel, WindowAspect, WindowAxis, window_anchor,
};
use crate::mod_path_key;
use crate::presentation::{
    self, ImportErrorKind, ImportFailure, ImportPresentation, ModImpactPresentation,
    ModRequirementPresentation, ThirdPartyLicenses, ToastStack, parse_network, validate_ip,
    validate_port,
};
use crate::server_property_form;
use crate::settings_drafts::{DraftField, SettingsDrafts, SettingsSave};
use crate::ui::{
    AccountRow, ActivityRow, AppWindow, BlockedFileRow, DiagnosticRow, DiskUsageRow, GuiI18n,
    HostView, ImpactRow, ImportDuplicateRow, ImportOptionRow, InstanceRow, IsolationCandidateRow,
    JavaRow, LicenseRow, LinkedTargetRow, LocalModClashRow, LogChunk, MenuRow, ModRow,
    ModSearchRow, ModVersionRow, NameRules, PackContentRow, PendingEntityRow, PromoteServerRow,
    PropertyRules, RelationRow, RunningRow, ServerPackRules, ServerPropertyGroup,
    ServerPropertyRow, ServerRow, SnapshotRow, Theme, WindowSizePreview, WorkflowActionRow,
};
use crate::window_aspect;
use crate::workspace_layout::{PaneId, Revealed, Surface, WorkspaceLayout};

mod diagnosis_flow;
mod instance_list;
mod java_runtime;
mod log_views;
mod mod_views;
mod server_tools;
mod storage_maintenance;
mod workspace;

use diagnosis_flow::DiagnosisFlowState;
use instance_list::{InstanceListState, Listed, attach_icon_pool, host_icon_slot};
use log_views::LogViews;
use mod_views::ModViews;
use server_tools::{LinkedClaim, LinkedRefusal, ServerAction, ServerTools};
use workspace::{PaneChange, WorkspaceState};

pub struct Controller {
    _poll_timer: Timer,
    /// A slower poll of its own, so watching what is running does not add work
    /// to every other screen. Runs only while that page is open.
    _running_timer: Timer,
    /// Notices a new local date that starts or ends a season, so a season's
    /// theme comes and goes while Ephervex stays open.
    _season_timer: Timer,
    /// Kept so a later hook, such as [`accept_file_drops`], can send the same
    /// commands the callbacks registered here do.
    commands: Sender<Command>,
    import_flow: Rc<RefCell<ImportFlowState>>,
    workspace: Rc<RefCell<WorkspaceState>>,
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
    /// The source grew. Every view redraws; one the user has scrolled away
    /// from also counts the rows that appeared below its position.
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

/// What bringing the rows to a new set of chunks did to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChunkChange {
    /// Rows removed from the front, which moved every row that is left up by
    /// as many.
    dropped_front: usize,
    /// The first row, counted after the rows were dropped, that is not the
    /// row it was before. Rows from here on are new or changed.
    first_changed: usize,
    /// How many rows there are now.
    rows: usize,
}

impl ChunkChange {
    /// The rows that appeared or changed at the end.
    const fn new_rows(&self) -> usize {
        self.rows.saturating_sub(self.first_changed)
    }
}

/// Brings the rows to `chunks`, and says what changed, or `None` when nothing
/// did. Rows before `first_changed` are exactly as they were, apart from
/// moving up by `dropped_front`, so a selection that stays among them still
/// names the same text.
fn sync_log_chunks(model: &VecModel<LogChunk>, chunks: Vec<LogChunk>) -> Option<ChunkChange> {
    let current = (0..model.row_count())
        .filter_map(|index| model.row_data(index))
        .collect::<Vec<_>>();
    if current.len() == chunks.len()
        && current
            .iter()
            .zip(&chunks)
            .all(|(left, right)| same_chunk(left, right))
    {
        return None;
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
                return Some(ChunkChange {
                    dropped_front: start,
                    first_changed: overlap,
                    rows: model.row_count(),
                });
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
    Some(ChunkChange {
        dropped_front: 0,
        first_changed: common_prefix,
        rows: model.row_count(),
    })
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

    let blocked_rows: Vec<BlockedFileRow> = state
        .blocked
        .iter()
        .map(|blocked| BlockedFileRow {
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
        let Some(request_id) = register_import_creation(
            &mut submit_state.borrow_mut(),
            &mut submit_creations.borrow_mut(),
            &submission,
            persistent,
        ) else {
            // The same archive is already importing under a card of its own:
            // refuse as the other creation dialogs do. The dialog stays open
            // and usable.
            if let Some(ui) = weak.upgrade() {
                ui.invoke_present_toast(lookup("gui-creation-already-running").into(), true);
                render_import_flow(&ui, &submit_state.borrow());
            }
            return;
        };
        if let Some(ui) = weak.upgrade() {
            ui.set_modal(SharedString::default());
            ui.set_modal_error(SharedString::default());
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

/// Gives a prepared import submission its pending card. `None` means the same
/// archive is already importing: the flow leaves `Submitting` so the dialog
/// is not stuck, and no import may be started.
fn register_import_creation(
    flow: &mut ImportFlowState,
    creations: &mut CreationTracker,
    submission: &PreparedImportSubmission,
    persistent: bool,
) -> Option<String> {
    let key = CreationKey::Import {
        archive: submission.archive.display().to_string(),
    };
    let request_id = creations.accept(key.clone(), pending_creation(&key, persistent));
    if request_id.is_none() {
        flow.submit_finished(submission.generation);
    }
    request_id
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

    /// Re-identifies an import's card as the linked server it is now creating
    /// for `instance`, so the work is shown on that instance's row instead of
    /// as a second entry beside it. The request, and with it the key that
    /// refuses a repeated click on the archive, stays the same.
    fn hand_off_to_linked_server(&mut self, request_id: &str, instance: &str, server_name: &str) {
        let Some(pending) = self
            .pending
            .iter_mut()
            .find(|pending| pending.request_id == request_id)
        else {
            return;
        };
        pending.server = true;
        instance.clone_into(&mut pending.linked_client_id);
        server_name.clone_into(&mut pending.name);
        pending.target = lookup("gui-linked-client");
        pending.detail.clear();
        pending.progress = 0.0;
        pending.indeterminate = true;
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
    /// Whether `instance` names a server rather than a client.
    server: bool,
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
    /// Queued requests that show their plan before installing, however little
    /// it leaves to decide, because what asked for them did not name the
    /// release, as Diagnostics does not.
    confirmed: Vec<BrowserInstall>,
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

    /// The same, for a request that shows its plan before installing. A request
    /// already queued keeps the way it was asked for.
    fn enqueue_confirmed(&mut self, request: BrowserInstall) -> Option<BrowserInstall> {
        if self.active.as_ref() != Some(&request) && !self.waiting.contains(&request) {
            self.confirmed.push(request.clone());
        }
        self.enqueue(request)
    }

    /// Whether `request` shows its plan before installing.
    fn confirms(&self, request: &BrowserInstall) -> bool {
        self.confirmed.contains(request)
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
        self.confirmed.retain(|confirmed| confirmed != request);
        self.active = self.waiting.pop_front();
        self.active.clone()
    }

    /// Retires whatever is running, whether or not its identity is known.
    ///
    /// For the endings that arrive without one, such as a confirmation the user
    /// closed.
    fn finish_active(&mut self) -> Option<BrowserInstall> {
        let finished = self.active.take()?;
        self.confirmed.retain(|confirmed| *confirmed != finished);
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

/// Where a mod plan was asked for, which decides where it may be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanAskedFrom {
    /// The Manage dialog, in the opening numbered here.
    Manager(u64),
    /// A mod card, with no dialog open.
    Card,
}

/// The one ask of a kind that a mod plan is still due for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AwaitedPlan {
    request: u64,
    asked_from: PlanAskedFrom,
}

/// Which plans a dialog's request may still be answered with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanKind {
    Removal,
    Apply,
}

/// Whether a plan that answers the awaited ask may be shown now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanAdmission {
    Show,
    /// The Manage dialog that asked has been left, so nobody is waiting.
    Unwanted,
    /// Another dialog is open that the plan must not replace.
    Blocked,
}

/// The mod plans asked for and not yet shown. A plan names its ask and is shown
/// once, only while the dialog that asked is still on screen: the host and path
/// a dialog addresses outlive it and other flows overwrite them.
#[derive(Debug, Default)]
struct ModPlanRequests {
    issued: Cell<u64>,
    /// How many times the Manage dialog has been opened for a mod.
    manager_openings: Cell<u64>,
    removal: RefCell<Option<AwaitedPlan>>,
    apply: RefCell<Option<AwaitedPlan>>,
}

impl ModPlanRequests {
    fn slot(&self, kind: PlanKind) -> &RefCell<Option<AwaitedPlan>> {
        match kind {
            PlanKind::Removal => &self.removal,
            PlanKind::Apply => &self.apply,
        }
    }

    fn manager_opened(&self) {
        self.manager_openings
            .set(self.manager_openings.get().wrapping_add(1));
    }

    /// Records an ask made over `modal`; a newer ask replaces the older one.
    fn ask(&self, kind: PlanKind, modal: &str) -> u64 {
        let request = self.issued.get().wrapping_add(1);
        self.issued.set(request);
        let asked_from = if modal == "mod-manage" {
            PlanAskedFrom::Manager(self.manager_openings.get())
        } else {
            PlanAskedFrom::Card
        };
        *self.slot(kind).borrow_mut() = Some(AwaitedPlan {
            request,
            asked_from,
        });
        request
    }

    /// Takes the awaited ask `request` names; `None` once replaced or answered.
    fn answer(&self, kind: PlanKind, request: u64) -> Option<AwaitedPlan> {
        let mut slot = self.slot(kind).borrow_mut();
        if slot.as_ref().is_some_and(|ask| ask.request == request) {
            slot.take()
        } else {
            None
        }
    }

    fn admission(&self, awaited: &AwaitedPlan, modal: &str) -> PlanAdmission {
        match awaited.asked_from {
            PlanAskedFrom::Manager(opening) if opening != self.manager_openings.get() => {
                PlanAdmission::Unwanted
            }
            PlanAskedFrom::Manager(_) => match modal {
                "mod-manage" => PlanAdmission::Show,
                // A download prompt took the dialog the user did not leave.
                "mod-acquire" => PlanAdmission::Blocked,
                _ => PlanAdmission::Unwanted,
            },
            PlanAskedFrom::Card if modal.is_empty() => PlanAdmission::Show,
            PlanAskedFrom::Card => PlanAdmission::Blocked,
        }
    }

    /// The ask a plan answers when it may be shown now; a card's ask that is
    /// refused is announced, the rest are dropped.
    fn admit(&self, ui: &AppWindow, kind: PlanKind, request: u64) -> Option<AwaitedPlan> {
        let awaited = self.answer(kind, request)?;
        match self.admission(&awaited, ui.get_modal().as_str()) {
            PlanAdmission::Show => Some(awaited),
            PlanAdmission::Unwanted => None,
            PlanAdmission::Blocked => {
                let key = if ui.get_modal() == "mod-acquire" {
                    "gui-dialog-not-shown"
                } else {
                    "gui-dialog-not-shown-open-dialog"
                };
                ui.invoke_present_toast(lookup(key).into(), true);
                None
            }
        }
    }
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

/// A change to a host's mods that isolation has asked the worker for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IsolationMutation {
    Start,
    Answer,
    Undo,
    Finish,
    Cancel,
    Recover,
}

impl IsolationMutation {
    /// What the Isolation surface says while the change runs.
    const fn busy_message(self) -> &'static str {
        match self {
            Self::Start => "operation-starting-isolation",
            Self::Answer => "gui-isolation-busy-answer",
            Self::Undo => "gui-isolation-busy-undo",
            Self::Finish => "gui-isolation-busy-finish",
            Self::Cancel => "operation-canceling-isolation",
            Self::Recover => "gui-isolation-busy-recover",
        }
    }

    /// The reported operation whose current stage replaces the fixed message.
    const fn operation(self) -> Option<&'static str> {
        match self {
            Self::Start => Some("operation-starting-isolation"),
            Self::Cancel => Some("operation-canceling-isolation"),
            Self::Answer | Self::Undo | Self::Finish | Self::Recover => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingIsolation {
    request: u64,
    host: InstalledHost,
    mutation: IsolationMutation,
}

/// One host's isolation setup, as far as this window has it.
#[derive(Debug, Default)]
struct IsolationSetup {
    /// Which load of this host's isolation a late answer has to belong to.
    generation: u64,
    pinned: HashSet<PathBuf>,
    candidates: Vec<backend::IsolationCandidateData>,
    query: String,
    /// The session's host does not have the round's mods yet, so nothing
    /// that depends on them may be sent.
    unreconciled: bool,
    /// The plan fingerprint the setup on screen was computed from.
    setup_basis: Option<u64>,
    /// The phase the host's surface shows.
    phase: String,
}

impl IsolationSetup {
    /// Drops the candidates and pins of a setup whose plan no longer describes the host.
    fn forget_setup(&mut self) {
        self.setup_basis = None;
        self.pinned.clear();
        self.candidates.clear();
        self.query.clear();
    }
}

/// Isolation as the window follows it: one setup per shown host, and the one
/// isolation change the worker has not finished, whichever host it is for.
#[derive(Debug, Default)]
struct IsolationFlowState {
    last_generation: u64,
    hosts: HashMap<InstalledHost, IsolationSetup>,
    last_request: u64,
    /// The one isolation change the worker has not finished. A second click,
    /// from any view of any host, is refused here before a second command can
    /// be queued behind it.
    pending: Option<PendingIsolation>,
}

impl IsolationFlowState {
    /// Starts a fresh load of `host`'s isolation, answering its generation.
    fn begin(&mut self, host: InstalledHost) -> u64 {
        self.last_generation = self.last_generation.wrapping_add(1);
        let setup = self.hosts.entry(host).or_default();
        setup.generation = self.last_generation;
        setup.pinned.clear();
        setup.candidates.clear();
        setup.query.clear();
        self.last_generation
    }

    /// Whether an answer about `host` belongs to its current load. Zero is an
    /// answer nothing on screen asked for, such as a reload after a change.
    fn accepts(&self, host: InstalledHost, generation: u64) -> bool {
        generation == 0
            || self
                .hosts
                .get(&host)
                .is_some_and(|setup| setup.generation == generation)
    }

    fn setup(&self, host: InstalledHost) -> Option<&IsolationSetup> {
        self.hosts.get(&host)
    }

    fn setup_mut(&mut self, host: InstalledHost) -> Option<&mut IsolationSetup> {
        self.hosts.get_mut(&host)
    }

    /// The generation `host`'s current load answers to, starting one when the
    /// window has none, as ending a session from its workflow banner does.
    fn generation_of(&mut self, host: InstalledHost) -> u64 {
        match self.hosts.get(&host) {
            Some(setup) => setup.generation,
            None => self.begin(host),
        }
    }

    /// Forgets the setups of hosts the workspace no longer shows.
    fn follow(&mut self, shown: &[InstalledHost]) {
        self.hosts.retain(|host, _| shown.contains(host));
    }

    /// Claims the single in-flight isolation change, or refuses while one is pending.
    fn dispatch(&mut self, host: InstalledHost, mutation: IsolationMutation) -> Option<u64> {
        if self.pending.is_some() {
            return None;
        }
        self.last_request = self.last_request.wrapping_add(1);
        self.pending = Some(PendingIsolation {
            request: self.last_request,
            host,
            mutation,
        });
        Some(self.last_request)
    }

    /// Releases the in-flight change the worker reports finished.
    fn settle(&mut self, request: u64) -> bool {
        let settled = self
            .pending
            .is_some_and(|pending| pending.request == request);
        if settled {
            self.pending = None;
        }
        settled
    }

    /// Forgets a setup that never started, so a late plan cannot bring it back.
    fn discard(&mut self, host: InstalledHost) {
        self.last_generation = self.last_generation.wrapping_add(1);
        let generation = self.last_generation;
        let setup = self.hosts.entry(host).or_default();
        setup.generation = generation;
        setup.forget_setup();
        "none".clone_into(&mut setup.phase);
    }
}

/// The one disk usage measurement the view can be waiting for.
#[derive(Debug, Default)]
struct DiskUsageState {
    last_request: u64,
    pending: Option<u64>,
    /// The measurement's operation, once it reports, which is what Stop cancels.
    operation: Option<String>,
    /// Whether the view holds a finished measurement.
    measured: bool,
    /// The maintenance section's own state, kept beside the measurement it
    /// can make out of date.
    maintenance: storage_maintenance::MaintenanceState,
}

const fn storage_category_messages(category: StorageCategory) -> (&'static str, &'static str) {
    match category {
        StorageCategory::ClientInstances => (
            "gui-disk-usage-client-instances",
            "gui-disk-usage-client-instances-detail",
        ),
        StorageCategory::LocalServers => (
            "gui-disk-usage-local-servers",
            "gui-disk-usage-local-servers-detail",
        ),
        StorageCategory::Mods => ("gui-disk-usage-mods", "gui-disk-usage-mods-detail"),
        StorageCategory::Snapshots => (
            "gui-disk-usage-snapshots",
            "gui-disk-usage-snapshots-detail",
        ),
        StorageCategory::Logs => ("gui-disk-usage-logs", "gui-disk-usage-logs-detail"),
        StorageCategory::DownloadCache => (
            "gui-disk-usage-download-cache",
            "gui-disk-usage-download-cache-detail",
        ),
        StorageCategory::SharedGameFiles => (
            "gui-disk-usage-shared-game-files",
            "gui-disk-usage-shared-game-files-detail",
        ),
        StorageCategory::JavaRuntimes => (
            "gui-disk-usage-java-runtimes",
            "gui-disk-usage-java-runtimes-detail",
        ),
        StorageCategory::LauncherState => (
            "gui-disk-usage-launcher-state",
            "gui-disk-usage-launcher-state-detail",
        ),
        StorageCategory::Recovery => ("gui-disk-usage-recovery", "gui-disk-usage-recovery-detail"),
        StorageCategory::Other => ("gui-disk-usage-other", "gui-disk-usage-other-detail"),
    }
}

const fn storage_retention_messages(retention: StorageRetention) -> (&'static str, &'static str) {
    match retention {
        StorageRetention::Persistent => (
            "gui-disk-usage-retention-persistent",
            "gui-disk-usage-retention-persistent-detail",
        ),
        StorageRetention::Temporary => (
            "gui-disk-usage-retention-temporary",
            "gui-disk-usage-retention-temporary-detail",
        ),
        StorageRetention::Regenerable => (
            "gui-disk-usage-retention-regenerable",
            "gui-disk-usage-retention-regenerable-detail",
        ),
        StorageRetention::Recovery => (
            "gui-disk-usage-retention-recovery",
            "gui-disk-usage-retention-recovery-detail",
        ),
    }
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn share_of(bytes: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (bytes as f64 / total as f64) as f32
    }
}

/// Shows a finished measurement, if it is the one the view last asked for.
fn apply_disk_usage(
    ui: &AppWindow,
    state: &mut DiskUsageState,
    request: u64,
    report: &ephervex_core::DiskUsageReport,
) {
    if request != state.last_request {
        return;
    }
    let total = report.total_bytes;
    let size = |bytes: u64| SharedString::from(ephervex_model::format_binary_bytes(bytes));
    let retention = report
        .retention
        .iter()
        .map(|usage| {
            let (label, detail) = storage_retention_messages(usage.retention);
            DiskUsageRow {
                id: usage.retention.as_str().into(),
                label: lookup(label).into(),
                detail: lookup(detail).into(),
                size: size(usage.bytes),
                fraction: share_of(usage.bytes, total),
                locations: string_model(Vec::new()),
            }
        })
        .collect();
    // Largest first; a category holding nothing is still listed, so the list
    // always adds up to the total above it.
    let mut categories: Vec<&ephervex_core::CategoryUsage> = report.categories.iter().collect();
    categories.sort_by_key(|usage| std::cmp::Reverse(usage.bytes));
    let categories = categories
        .into_iter()
        .map(|usage| {
            let (label, detail) = storage_category_messages(usage.category);
            DiskUsageRow {
                id: usage.category.as_str().into(),
                label: lookup(label).into(),
                detail: lookup(detail).into(),
                size: size(usage.bytes),
                fraction: share_of(usage.bytes, total),
                locations: string_model(
                    usage
                        .locations
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect(),
                ),
            }
        })
        .collect();
    state.measured = true;
    ui.set_disk_usage_total(size(total));
    ui.set_disk_usage_measured_at(
        lookup_args(
            "gui-disk-usage-measured-at",
            [(
                "value",
                Arg::from(
                    report
                        .measured_at
                        .with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string(),
                ),
            )],
        )
        .into(),
    );
    ui.set_disk_usage_retention(model(retention));
    ui.set_disk_usage_categories(model(categories));
    ui.set_disk_usage_incomplete(report.is_incomplete());
    ui.set_disk_usage_incomplete_text(
        lookup_args(
            "gui-disk-usage-incomplete",
            [(
                "count",
                Arg::from(i64::try_from(report.unreadable).unwrap_or(i64::MAX)),
            )],
        )
        .into(),
    );
    ui.set_disk_usage_unreadable(string_model(
        report
            .unreadable_examples
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
    ));
    ui.set_disk_usage_state("measured".into());
}

fn settle_disk_usage(ui: &AppWindow, state: &mut DiskUsageState, request: u64) {
    if state.pending != Some(request) {
        return;
    }
    state.pending = None;
    state.operation = None;
    // A measurement that ended without a report, stopped or failed, leaves
    // whatever was shown before it.
    if ui.get_disk_usage_state() == "measuring" {
        ui.set_disk_usage_state(if state.measured { "measured" } else { "idle" }.into());
    }
}

/// Marks the shown measurement as describing a disk that has since changed.
///
/// Cleanup and recovery change what the stores hold, so figures measured before
/// them are not presented as current: they are taken off the screen and the
/// view says why, and a measurement still running is discarded, because it may
/// have read the disk half way through the change.
fn invalidate_disk_usage(ui: &AppWindow, state: &mut DiskUsageState) {
    if !state.measured && state.pending.is_none() {
        return;
    }
    state.last_request = state.last_request.wrapping_add(1);
    state.measured = false;
    ui.set_disk_usage_stale(true);
    // A measurement still running keeps showing as running, with its Stop, until
    // it ends and is settled; only its result is dropped.
    if state.pending.is_none() {
        ui.set_disk_usage_state("idle".into());
    }
}

fn apply_disk_usage_activity(ui: &AppWindow, state: &mut DiskUsageState, activity: &ActivityData) {
    storage_maintenance::apply_activity(ui, &mut state.maintenance, activity);
    if state.pending.is_none()
        || activity.item.title != lookup("operation-measuring-disk-usage")
        || !matches!(activity.item.status_kind, "queued" | "running")
    {
        return;
    }
    state.operation = Some(activity.item.id.clone());
    if !activity.item.detail.is_empty() {
        ui.set_disk_usage_progress(activity.item.detail.clone().into());
    }
}

fn register_disk_usage_actions(
    ui: &AppWindow,
    commands: &Sender<Command>,
    state: &Rc<RefCell<DiskUsageState>>,
) {
    let sender = commands.clone();
    let measuring = Rc::clone(state);
    let weak = ui.as_weak();
    ui.on_measure_disk_usage(move || {
        let mut state = measuring.borrow_mut();
        if state.pending.is_some() {
            return;
        }
        state.last_request = state.last_request.wrapping_add(1);
        state.pending = Some(state.last_request);
        if let Some(ui) = weak.upgrade() {
            ui.set_disk_usage_stale(false);
            ui.set_disk_usage_state("measuring".into());
            ui.set_disk_usage_progress(SharedString::default());
        }
        send(
            &sender,
            Command::MeasureDiskUsage {
                request: state.last_request,
            },
        );
    });

    let sender = commands.clone();
    let stopping = Rc::clone(state);
    ui.on_stop_disk_usage(move || {
        if let Some(operation) = stopping.borrow().operation.clone() {
            send(&sender, Command::CancelOperation(operation));
        }
    });

    let sender = commands.clone();
    ui.on_open_storage_location(move |id, index| {
        let category = StorageCategory::ALL
            .into_iter()
            .find(|category| category.as_str() == id.as_str());
        if let (Some(category), Ok(index)) = (category, usize::try_from(index)) {
            send(&sender, Command::OpenStorageLocation { category, index });
        }
    });

    storage_maintenance::register(ui, commands, state);
}

/// A restart this controller asked for, and where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TrackedRestart {
    request: u64,
    stage: backend::RestartStage,
}

/// Lifecycle state that belongs to a host rather than to any view of it.
///
/// Every view reads it from the host's own row, so a host shown in several
/// places shows one restart, and a second request from any of them is refused
/// here before it can reach the worker.
#[derive(Debug, Default)]
struct HostLifecycle {
    servers: ServerTools,
    last_request: u64,
    /// Keyed by host kind and id.
    restarts: HashMap<String, TrackedRestart>,
    /// Clients whose running launch this run started, and which a restart
    /// therefore knows how to start again.
    restartable: HashSet<String>,
    /// Pair starts sent to the worker and not yet settled.
    pair_starts: Vec<PairStart>,
}

/// One start of a pair that is under way, identified by its operation so a
/// late settle of an earlier one changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PairStart {
    operation: u64,
    client: InstalledHost,
    server: InstalledHost,
}

fn lifecycle_key(host: InstalledHost) -> String {
    format!("{}:{}", host.kind(), host.id())
}

impl HostLifecycle {
    /// Claims a restart of `host`, or refuses while one, or a start of its
    /// pair, is under way.
    fn request(&mut self, host: InstalledHost) -> Option<u64> {
        let key = lifecycle_key(host);
        if matches!(host, InstalledHost::Server(id) if self.servers.busy(id))
            || self.restarts.contains_key(&key)
            || self.pair_starting(host)
        {
            return None;
        }
        self.last_request = self.last_request.wrapping_add(1);
        self.restarts.insert(
            key,
            TrackedRestart {
                request: self.last_request,
                stage: backend::RestartStage::Stopping,
            },
        );
        Some(self.last_request)
    }

    /// Takes a stage the worker reported. A stage for a restart this
    /// controller is not tracking, or an earlier one, changes nothing.
    fn apply(
        &mut self,
        host: InstalledHost,
        request: u64,
        stage: Option<backend::RestartStage>,
    ) -> bool {
        let key = lifecycle_key(host);
        let Some(tracked) = self.restarts.get_mut(&key) else {
            return false;
        };
        // Zero is the server reporting its own stage, which belongs to the
        // restart already under way.
        if request != 0 && request != tracked.request {
            return false;
        }
        match stage {
            Some(stage) => tracked.stage = stage,
            None if request != 0 => {
                self.restarts.remove(&key);
            }
            None => return false,
        }
        true
    }

    /// Claims a start of the pair `client` and `server` form, or refuses
    /// while one that includes either of them is under way.
    fn begin_pair_start(&mut self, client: InstalledHost, server: InstalledHost) -> Option<u64> {
        if self.pair_starting(client) || self.pair_starting(server) {
            return None;
        }
        self.last_request = self.last_request.wrapping_add(1);
        self.pair_starts.push(PairStart {
            operation: self.last_request,
            client,
            server,
        });
        Some(self.last_request)
    }

    /// Ends the pair start the worker settled. An operation this controller
    /// is not tracking changes nothing.
    fn settle_pair_start(&mut self, operation: u64, server_completion_owned: bool) -> bool {
        let before = self.pair_starts.len();
        if let Some(start) = self
            .pair_starts
            .iter()
            .find(|start| start.operation == operation)
            && !server_completion_owned
            && let InstalledHost::Server(server) = start.server
        {
            self.servers
                .apply(server, backend::ServerToolEvent::StartAbandoned);
        }
        self.pair_starts
            .retain(|start| start.operation != operation);
        self.pair_starts.len() != before
    }

    /// The server of a pair start still being tracked.
    fn pair_start_server(&self, operation: u64) -> Option<ephervex_model::ServerId> {
        self.pair_starts
            .iter()
            .find(|start| start.operation == operation)
            .and_then(|start| match start.server {
                InstalledHost::Server(server) => Some(server),
                InstalledHost::Instance(_) => None,
            })
    }

    fn pair_starting(&self, host: InstalledHost) -> bool {
        self.pair_starts
            .iter()
            .any(|start| start.client == host || start.server == host)
    }

    fn stage_of(&self, host: InstalledHost) -> &'static str {
        self.restarts
            .get(&lifecycle_key(host))
            .map_or("", |tracked| tracked.stage.as_str())
    }

    /// Writes a client's restart stage and restartability onto its row.
    fn stamp_client(&self, row: &mut InstanceRow) -> bool {
        let Some(instance) = backend::parse_instance_id(&row.id) else {
            return false;
        };
        let host = InstalledHost::Instance(instance);
        let stage = self.stage_of(host);
        let can_restart = self.restartable.contains(&instance.to_string());
        let pair_starting = self.pair_starting(host);
        let changed = row.restart_stage != stage
            || row.can_restart != can_restart
            || row.pair_starting != pair_starting;
        row.restart_stage = stage.into();
        row.can_restart = can_restart;
        row.pair_starting = pair_starting;
        changed
    }

    /// Writes a server's restart stage onto its row.
    fn stamp_server(&self, row: &mut ServerRow) -> bool {
        let Some(server) = backend::parse_server_id(&row.id) else {
            return false;
        };
        let host = InstalledHost::Server(server);
        let stage = self.stage_of(host);
        let before = row.clone();
        row.restart_stage = stage.into();
        row.pair_starting = self.pair_starting(host);
        self.servers.stamp(server, row, !stage.is_empty());
        *row != before
    }

    /// Writes the paired host's state onto a host view. A client's paired
    /// server is one of its linked servers, which are stamped already; a
    /// server's paired client keeps the state its row had at render.
    fn stamp_pair(&self, view: &mut HostView) -> bool {
        let before = (
            view.pair_state.clone(),
            view.pair_state_kind.clone(),
            view.pair_restart_stage.clone(),
        );
        if view.kind == "client" {
            let paired = view
                .linked_servers
                .iter()
                .find(|row| !row.id.is_empty() && row.id == view.client.paired_server_id);
            (
                view.pair_state,
                view.pair_state_kind,
                view.pair_restart_stage,
            ) = paired.map_or_else(Default::default, |row| {
                (row.state, row.state_kind, row.restart_stage)
            });
        } else {
            view.pair_restart_stage = backend::parse_instance_id(&view.server.paired_client_id)
                .map_or("", |client| self.stage_of(InstalledHost::Instance(client)))
                .into();
        }
        before
            != (
                view.pair_state.clone(),
                view.pair_state_kind.clone(),
                view.pair_restart_stage.clone(),
            )
    }
}

/// Only classified failures enter the existing quick Diagnostics path.
fn apply_server_tool_event(
    ui: &AppWindow,
    workspace: &Workspace,
    server: ephervex_model::ServerId,
    event: backend::ServerToolEvent,
) {
    if matches!(event, backend::ServerToolEvent::Started) {
        workspace.forget_last_run(InstalledHost::Server(server));
    }
    // A start may have installed the managed runtime it planned, whether it
    // then came up, failed or was abandoned.
    if matches!(
        event,
        backend::ServerToolEvent::Started
            | backend::ServerToolEvent::StartAbandoned
            | backend::ServerToolEvent::RestartFailed { .. }
            | backend::ServerToolEvent::Finished { .. }
            | backend::ServerToolEvent::EulaRefused { .. }
    ) {
        java_runtime::reload_shown(workspace);
    }
    if matches!(
        event,
        backend::ServerToolEvent::Started
            | backend::ServerToolEvent::StartAbandoned
            | backend::ServerToolEvent::RestartFailed { .. }
            | backend::ServerToolEvent::Finished { .. }
            | backend::ServerToolEvent::EulaRefused { .. }
    ) {
        workspace
            .logs
            .borrow_mut()
            .settle_start(&LiveLogSource::Server(server));
    }
    if let backend::ServerToolEvent::EulaRefused { message } = &event {
        diagnosis_flow::apply_start_refused(
            workspace,
            &mut workspace.diagnosis.borrow_mut(),
            InstalledHost::Server(server),
            message.clone(),
        );
    }
    {
        let mut lifecycle = workspace.lifecycle.borrow_mut();
        if let Some(failure) = lifecycle.servers.apply(server, event)
            && let Some(revision) = lifecycle.servers.failure_revision(server)
        {
            send(
                &workspace.commands,
                Command::ExplainServerFailure {
                    server,
                    failure,
                    revision,
                },
            );
        }
    }
    workspace.refresh(ui);
    stamp_host_lifecycle(ui, &workspace.lifecycle.borrow());
}

/// Stamps each host row with its restart stage, and each client row with
/// whether it can be restarted, in every list that shows one.
fn stamp_host_lifecycle(ui: &AppWindow, lifecycle: &HostLifecycle) {
    stamp_rows(&ui.get_instances(), |row: &mut InstanceRow| {
        lifecycle.stamp_client(row)
    });
    stamp_rows(&ui.get_global_servers(), |row: &mut ServerRow| {
        lifecycle.stamp_server(row)
    });
    let views = ui.get_host_views();
    stamp_rows(&views, |view: &mut HostView| {
        if view.kind == "client" {
            lifecycle.stamp_client(&mut view.client)
        } else {
            lifecycle.stamp_server(&mut view.server)
        }
    });
    for view in views.iter() {
        stamp_rows(&view.linked_servers, |row: &mut ServerRow| {
            lifecycle.stamp_server(row)
        });
    }
    stamp_rows(&views, |view: &mut HostView| lifecycle.stamp_pair(view));
}

/// Ends a pair start the worker settled, in every view of both sides.
fn apply_pair_start_settled(
    ui: &AppWindow,
    workspace: &Workspace,
    lifecycle: &RefCell<HostLifecycle>,
    operation: u64,
    server_completion_owned: bool,
) {
    // A settled start that never ran the server leaves its console waiting for
    // a run that is not coming.
    if !server_completion_owned
        && let Some(server) = lifecycle.borrow().pair_start_server(operation)
    {
        workspace
            .logs
            .borrow_mut()
            .settle_start(&LiveLogSource::Server(server));
    }
    if lifecycle
        .borrow_mut()
        .settle_pair_start(operation, server_completion_owned)
    {
        stamp_host_lifecycle(ui, &lifecycle.borrow());
    }
}

fn stamp_rows<T: Clone + 'static>(list: &ModelRc<T>, mut stamp: impl FnMut(&mut T) -> bool) {
    for index in 0..list.row_count() {
        if let Some(mut row) = list.row_data(index)
            && stamp(&mut row)
        {
            list.set_row_data(index, row);
        }
    }
}

fn register_restart_actions(
    ui: &AppWindow,
    commands: &Sender<Command>,
    lifecycle: &Rc<RefCell<HostLifecycle>>,
) {
    let restart = {
        let sender = commands.clone();
        let lifecycle = Rc::clone(lifecycle);
        let weak = ui.as_weak();
        move |host: InstalledHost| {
            let Some(request) = lifecycle.borrow_mut().request(host) else {
                return;
            };
            if let Some(ui) = weak.upgrade() {
                stamp_host_lifecycle(&ui, &lifecycle.borrow());
            }
            send(&sender, Command::RestartHost { host, request });
        }
    };
    let client = restart.clone();
    ui.on_client_restart(move |id| {
        if let Some(instance) = backend::parse_instance_id(&id) {
            client(InstalledHost::Instance(instance));
        }
    });
    ui.on_server_restart(move |id| {
        if let Some(server) = backend::parse_server_id(&id) {
            restart(InstalledHost::Server(server));
        }
    });
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

    /// Whether `generation` still names the attempt this flow is tracking.
    fn tracks(&self, generation: u64) -> bool {
        self.tracked_generation == Some(generation)
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
    Mods(InstalledHost),
    /// Nothing. A file let go over a confirmation the user is in the middle of
    /// is far more likely to be a misplaced gesture than a request to abandon
    /// it. So is one let go with the Mods lists of two hosts on screen, which
    /// is not a pack either.
    Ignored,
}

/// Which field, if any, a drop belongs to right now.
fn drop_intent(ui: &AppWindow, workspace: &WorkspaceState) -> DropIntent {
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
        "" => match mods_lists_on_screen(ui, workspace) {
            ModsListsOnScreen::None => DropIntent::Import,
            ModsListsOnScreen::One(host) => DropIntent::Mods(host),
            ModsListsOnScreen::Several => DropIntent::Ignored,
        },
        "import" => DropIntent::Import,
        _ => DropIntent::Ignored,
    }
}

/// Which hosts' Mods lists are visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModsListsOnScreen {
    None,
    One(InstalledHost),
    /// Lists of two different hosts. A drop carries no position, so there is no
    /// telling which one it was meant for, and it is not a pack either.
    Several,
}

fn mods_lists_on_screen(ui: &AppWindow, workspace: &WorkspaceState) -> ModsListsOnScreen {
    if !ui.get_workspace_open() || !matches!(ui.get_page(), INSTANCES_PAGE | SERVERS_PAGE) {
        return ModsListsOnScreen::None;
    }
    let Some(layout) = workspace.layout() else {
        return ModsListsOnScreen::None;
    };
    let mut hosts = layout
        .panes()
        .into_iter()
        .filter(|placed| placed.pane.surface() == Surface::Mods)
        .map(|placed| placed.pane.host());
    let Some(host) = hosts.next() else {
        return ModsListsOnScreen::None;
    };
    if hosts.all(|other| other == host) {
        ModsListsOnScreen::One(host)
    } else {
        ModsListsOnScreen::Several
    }
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
    let workspace = Rc::clone(&controller.workspace);
    drops.listen(move |drop| {
        let Some(ui) = weak.upgrade() else { return };
        let intent = drop_intent(&ui, &workspace.borrow());
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
                        choose_server_pack(
                            &ui,
                            &sender,
                            target,
                            paths.first().map(PathBuf::as_path),
                        );
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

#[allow(clippy::too_many_lines)]
pub fn attach(ui: &AppWindow, keys: &BindingCapture) -> Controller {
    let backend = backend::spawn();
    let receiver = Rc::new(RefCell::new(backend.events));
    let activities = Rc::new(RefCell::new(Vec::<ActivityData>::new()));
    let creations = Rc::new(RefCell::new(CreationTracker::default()));
    let import_flow = Rc::new(RefCell::new(ImportFlowState::default()));
    let minecraft = Rc::new(RefCell::new(MinecraftDefaultsModel::new()));
    let settings_drafts = Rc::new(RefCell::new(SettingsDrafts::default()));
    let browser = Rc::new(ModBrowser::default());
    let local_mods = Rc::new(RefCell::new(LocalModDrop::default()));
    let isolation_flow = Rc::new(RefCell::new(IsolationFlowState::default()));
    let diagnosis_flow = Rc::new(RefCell::new(DiagnosisFlowState::default()));
    let lifecycle = Rc::new(RefCell::new(HostLifecycle::default()));
    let disk_usage = Rc::new(RefCell::new(DiskUsageState::default()));
    let instance_list = Rc::new(RefCell::new(InstanceListState::default()));
    let workspace = Workspace {
        state: Rc::new(RefCell::new(WorkspaceState::default())),
        lifecycle: Rc::clone(&lifecycle),
        logs: Rc::new(RefCell::new(LogViews::new(backend.commands.clone()))),
        mods: Rc::default(),
        browser: Rc::clone(&browser),
        isolation: Rc::clone(&isolation_flow),
        diagnosis: Rc::clone(&diagnosis_flow),
        acquisition_prompts: Rc::default(),
        stand_in_prompts: Rc::default(),
        plan_requests: Rc::default(),
        properties: Rc::default(),
        commands: backend.commands.clone(),
    };
    register_i18n(ui);
    register_toasts(ui);
    register_callbacks(
        ui,
        &backend.commands,
        &creations,
        &import_flow,
        &minecraft,
        &settings_drafts,
        &local_mods,
        &lifecycle,
        &disk_usage,
        &instance_list,
        &workspace,
        keys,
    );

    let weak = ui.as_weak();
    let poll_receiver = Rc::clone(&receiver);
    let poll_activities = Rc::clone(&activities);
    let poll_creations = Rc::clone(&creations);
    let poll_import_flow = Rc::clone(&import_flow);
    let poll_minecraft = Rc::clone(&minecraft);
    let poll_settings_drafts = Rc::clone(&settings_drafts);
    let poll_local_mods = Rc::clone(&local_mods);
    let poll_lifecycle = Rc::clone(&lifecycle);
    let poll_disk_usage = Rc::clone(&disk_usage);
    let poll_instance_list = Rc::clone(&instance_list);
    let poll_workspace = workspace.clone();
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
        let mut log_changes = HashMap::new();
        let mut worker_gone = take_worker_stopped();
        loop {
            let event = match poll_receiver.borrow_mut().try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    worker_gone = true;
                    break;
                }
            };
            match event {
                Event::LiveLog(update) => {
                    poll_workspace
                        .logs
                        .borrow_mut()
                        .apply(&update, &mut log_changes);
                }
                other => apply_event(
                    &ui,
                    other,
                    &poll_activities,
                    &poll_creations,
                    &poll_import_flow,
                    &poll_minecraft,
                    &poll_settings_drafts,
                    &poll_local_mods,
                    &poll_lifecycle,
                    &poll_disk_usage,
                    &poll_instance_list,
                    &poll_workspace,
                    &poll_commands,
                ),
            }
        }
        if worker_gone {
            report_worker_stopped(&ui);
        }
        poll_workspace.logs.borrow_mut().settle(log_changes);
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
        if ui.get_page() == RUNNING_PAGE && !ui.get_workspace_open() {
            send(&running_commands, Command::LoadRunning);
        }
    });

    let season_weak = ui.as_weak();
    let season_commands = backend.commands.clone();
    let season_watch = RefCell::new(SeasonWatch {
        forced: ephervex_core::seasonal_theme::forced_season(),
        ..SeasonWatch::default()
    });
    let season_timer = Timer::default();
    season_timer.start(TimerMode::Repeated, SEASON_CHECK, move || {
        let Some(ui) = season_weak.upgrade() else {
            return;
        };
        let due = season_watch.borrow_mut().due(
            &ui.get_application_theme(),
            ui.get_seasonal_themes(),
            &ui.get_shown_theme(),
            chrono::Local::now().date_naive(),
        );
        if due {
            send(&season_commands, Command::Refresh);
        }
    });

    Controller {
        _poll_timer: poll_timer,
        _running_timer: running_timer,
        _season_timer: season_timer,
        commands: backend.commands,
        import_flow,
        workspace: workspace.state,
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

/// How often the local date is checked against the seasons. A season's theme
/// arrives within this long of midnight.
const SEASON_CHECK: Duration = Duration::from_secs(60);

/// Decides when a new local date changes the theme that should be shown.
#[derive(Debug, Default)]
struct SeasonWatch {
    /// The date a refresh was last asked for, so a refresh that does not
    /// change the theme is not asked for again until the next date.
    asked_on: Option<chrono::NaiveDate>,
    /// The season shown on every day in place of the date's, if any.
    forced: Option<ephervex_core::seasonal_theme::SeasonalTheme>,
}

impl SeasonWatch {
    /// Whether the settings should be read again because `today` shows a
    /// different theme than `shown`, by
    /// [`ephervex_core::seasonal_theme::ShownTheme::name`]. `preferred` is
    /// the recorded theme.
    fn due(
        &mut self,
        preferred: &str,
        seasonal_themes: bool,
        shown: &str,
        today: chrono::NaiveDate,
    ) -> bool {
        let Some(preferred) = ephervex_core::ApplicationTheme::from_stored(preferred) else {
            return false;
        };
        let wanted = ephervex_core::seasonal_theme::shown_theme_with(
            &preferred,
            seasonal_themes,
            self.forced,
            today,
        );
        if wanted.name() == shown || self.asked_on == Some(today) {
            return false;
        }
        self.asked_on = Some(today);
        true
    }
}

/// How often the stack is asked what has been shown for long enough.
///
/// One timer, running only while something is on screen, rather than a binding
/// per panel. A quarter second of slop on a dwell measured in seconds is not
/// visible, and this way an idle window wakes for nothing.
const TOAST_SWEEP: Duration = Duration::from_millis(250);

fn register_toasts(ui: &AppWindow) {
    let stack = Rc::new(RefCell::new(ToastStack::default()));
    let sweep = Rc::new(slint::Timer::default());

    let weak = ui.as_weak();
    let shown = Rc::clone(&stack);
    let ticking = Rc::clone(&sweep);
    ui.on_present_toast(move |message, error| {
        let Some(ui) = weak.upgrade() else { return };
        shown.borrow_mut().present(message, error, Instant::now());
        publish_toasts(&ui, &shown);
        start_toast_sweep(&ui, &shown, &ticking);
    });

    let weak = ui.as_weak();
    let shown = Rc::clone(&stack);
    ui.on_dismiss_toast(move |id| {
        let Some(ui) = weak.upgrade() else { return };
        shown.borrow_mut().dismiss(id, Instant::now());
        publish_toasts(&ui, &shown);
    });
}

/// Shows what the stack now holds.
fn publish_toasts(ui: &AppWindow, stack: &Rc<RefCell<ToastStack>>) {
    ui.set_toasts(model(stack.borrow().rows()));
}

/// Starts the sweep if it is not already running, and stops it once the stack
/// has emptied.
fn start_toast_sweep(ui: &AppWindow, stack: &Rc<RefCell<ToastStack>>, sweep: &Rc<slint::Timer>) {
    if sweep.running() {
        return;
    }
    let weak = ui.as_weak();
    let shown = Rc::clone(stack);
    let ticking = Rc::clone(sweep);
    sweep.start(slint::TimerMode::Repeated, TOAST_SWEEP, move || {
        let Some(ui) = weak.upgrade() else {
            ticking.stop();
            return;
        };
        shown.borrow_mut().expire(Instant::now());
        publish_toasts(&ui, &shown);
        if shown.borrow().is_empty() {
            ticking.stop();
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

/// Shows a newly chosen archive and, for the create-server dialog, starts
/// reading what it says about the selection it was made for.
fn choose_server_pack(
    ui: &AppWindow,
    commands: &Sender<Command>,
    target: ServerPackTarget,
    path: Option<&Path>,
) {
    set_server_pack(ui, target, path);
    if let (ServerPackTarget::NewServer, Some(path)) = (target, path) {
        send(
            commands,
            Command::InspectServerPack {
                path: path.to_path_buf(),
            },
        );
    }
}

/// Applies what the create-server dialog's server files say about their
/// selection.
///
/// A standalone server's fields are filled in from whatever the files name, and
/// the note says where those values came from. A linked server's selection is
/// its client's and is left alone. An answer for files the dialog no longer
/// shows is dropped.
fn apply_server_pack_evidence(
    ui: &AppWindow,
    path: &Path,
    evidence: Result<ServerArchiveEvidence, String>,
) {
    if ui.get_create_server_pack().as_str() != path.display().to_string() {
        return;
    }
    let evidence = match evidence {
        Ok(evidence) => evidence,
        Err(message) => {
            ui.set_create_server_pack_note(
                lookup_args(
                    "gui-server-pack-unreadable",
                    [("value", Arg::from(message.as_str()))],
                )
                .into(),
            );
            return;
        }
    };
    ui.set_create_server_pack_mods(evidence.carries_mods);
    ui.set_create_server_pack_loader(
        evidence
            .loader
            .map(|kind| kind.to_string())
            .unwrap_or_default()
            .into(),
    );
    ui.set_create_server_pack_loader_version(
        evidence.loader_version.clone().unwrap_or_default().into(),
    );
    ui.set_create_server_pack_minecraft(
        evidence
            .minecraft_version
            .clone()
            .unwrap_or_default()
            .into(),
    );
    if !ui.get_create_server_client_id().is_empty() {
        ui.set_create_server_pack_note(SharedString::default());
        return;
    }
    if let Some(minecraft) = &evidence.minecraft_version {
        ui.set_create_server_minecraft(minecraft.into());
        apply_minecraft_selection(ui, "server", minecraft);
    }
    if let Some(kind) = evidence.loader {
        ui.set_create_server_loader(kind.to_string().into());
        if let Some(version) = &evidence.loader_version {
            ui.set_create_server_loader_versions(model(vec![SharedString::from(version)]));
            ui.set_create_server_loader_version(version.into());
        } else {
            ui.set_create_server_loader_versions(model(Vec::new()));
            ui.set_create_server_loader_version(SharedString::default());
            ui.invoke_request_loader_catalog(
                "server".into(),
                kind.to_string().into(),
                ui.get_create_server_minecraft(),
            );
        }
    }
    recompute_create_server_name(ui);
    let note = match (
        &evidence.minecraft_version,
        evidence.loader,
        &evidence.loader_version,
    ) {
        (Some(minecraft), Some(loader), Some(version)) => lookup_args(
            "gui-server-pack-selection-read",
            [
                ("minecraft", Arg::from(minecraft.as_str())),
                ("loader", Arg::from(loader.to_string())),
                ("version", Arg::from(version.as_str())),
            ],
        ),
        _ if evidence.carries_mods || evidence.loader.is_some() => {
            lookup("gui-server-pack-selection-unknown")
        }
        _ => String::new(),
    };
    ui.set_create_server_pack_note(note.into());
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
            ui.set_create_server_pack_mods(false);
            ui.set_create_server_pack_minecraft(SharedString::default());
            ui.set_create_server_pack_loader(SharedString::default());
            ui.set_create_server_pack_loader_version(SharedString::default());
            ui.set_create_server_pack_note(SharedString::default());
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
    import_flow: &Rc<RefCell<ImportFlowState>>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    settings_drafts: &Rc<RefCell<SettingsDrafts>>,
    local_mods: &Rc<RefCell<LocalModDrop>>,
    lifecycle: &Rc<RefCell<HostLifecycle>>,
    disk_usage: &Rc<RefCell<DiskUsageState>>,
    instance_list: &Rc<RefCell<InstanceListState>>,
    workspace: &Workspace,
    keys: &BindingCapture,
) {
    register_instance_list(ui, commands, instance_list, lifecycle);
    register_catalog_actions(ui, commands);
    register_client_actions(ui, commands, creations);
    register_mod_actions(ui, commands, workspace);
    register_create_instance_naming(ui, commands);
    register_import_flow(ui, commands, import_flow, creations);
    register_server_pack_choice(ui, commands);
    register_create_server(ui, commands, Rc::clone(creations));

    let sender = commands.clone();
    ui.on_select_server(move |_| send(&sender, Command::Refresh));

    register_server_actions(ui, commands, workspace);
    register_workspace(ui, workspace);
    register_mod_state_actions(ui, commands, local_mods);
    register_snapshot_actions(ui, commands);
    register_isolation_actions(ui, commands, workspace);
    diagnosis_flow::register(ui, commands, workspace);
    java_runtime::register(ui, commands, workspace);
    register_restart_actions(ui, commands, lifecycle);
    register_disk_usage_actions(ui, commands, disk_usage);
    log_views::register(ui, &workspace.logs);
    register_settings(ui, commands, settings_drafts);
    register_minecraft_defaults(ui, commands, minecraft, keys);

    let weak = ui.as_weak();
    ui.on_copy_text(move |text| {
        if let Some(ui) = weak.upgrade() {
            copy_to_clipboard(&ui, text.into());
        }
    });

    let sender = commands.clone();
    ui.on_cancel_operation(move |id| send(&sender, Command::CancelOperation(id.into())));
}

/// The moment of capture, offered as a snapshot's label when none is typed.
///
/// Sorts chronologically as text, which is what a list of them is read in.
const SNAPSHOT_LABEL_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

fn register_snapshot_actions(ui: &AppWindow, commands: &Sender<Command>) {
    // Local time, because the label is read next to the host's own clock.
    let weak = ui.as_weak();
    ui.on_recompute_snapshot_label(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_snapshot_label_suggestion(
                chrono::Local::now()
                    .format(SNAPSHOT_LABEL_FORMAT)
                    .to_string()
                    .into(),
            );
        }
    });

    let sender = commands.clone();
    ui.on_load_snapshots(move |kind, id| {
        if let Some(host) = backend::parse_mod_host(&kind, &id) {
            send(&sender, Command::LoadSnapshots(host));
        }
    });

    let sender = commands.clone();
    ui.on_load_pack_content(move |kind, id| {
        if let Some(host) = backend::parse_mod_host(&kind, &id) {
            send(&sender, Command::LoadPackContent(host));
        }
    });

    let sender = commands.clone();
    ui.on_create_snapshot(move |kind, id, label| {
        if let Some(host) = backend::parse_mod_host(&kind, &id) {
            send(
                &sender,
                Command::CreateSnapshot {
                    host,
                    label: label.into(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_restore_snapshot(move |kind, id, snapshot, safety_snapshot| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let Some(snapshot) = backend::parse_snapshot_id(&snapshot) else {
            return;
        };
        send(
            &sender,
            Command::RestoreSnapshot {
                host,
                snapshot,
                safety_snapshot,
            },
        );
    });

    let sender = commands.clone();
    ui.on_rename_snapshot(move |kind, id, snapshot, label| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let Some(snapshot) = backend::parse_snapshot_id(&snapshot) else {
            return;
        };
        send(
            &sender,
            Command::RenameSnapshot {
                host,
                snapshot,
                label: label.into(),
            },
        );
    });

    let sender = commands.clone();
    ui.on_delete_snapshot(move |kind, id, snapshot| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let Some(snapshot) = backend::parse_snapshot_id(&snapshot) else {
            return;
        };
        send(&sender, Command::DeleteSnapshot { host, snapshot });
    });
}

#[allow(clippy::too_many_lines)]
fn register_isolation_actions(ui: &AppWindow, commands: &Sender<Command>, workspace: &Workspace) {
    let flow = &workspace.isolation;
    let sender = commands.clone();
    let state = Rc::clone(flow);
    ui.on_open_isolation(move |kind, id| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let generation = state.borrow_mut().begin(host);
        send(&sender, Command::OpenIsolation { host, generation });
    });

    let views = workspace.clone();
    ui.on_toggle_isolation_pin(move |kind, id, path, selected| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let mut flow = views.isolation.borrow_mut();
        let Some(setup) = flow.setup_mut(host) else {
            return;
        };
        let path = mod_path_key::decode(path.as_str());
        if selected {
            setup.pinned.insert(path);
        } else {
            setup.pinned.remove(&path);
        }
        publish_isolation_candidates(&views, host, setup);
    });

    let views = workspace.clone();
    ui.on_filter_isolation_candidates(move |kind, id, query| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let mut flow = views.isolation.borrow_mut();
        let Some(setup) = flow.setup_mut(host) else {
            return;
        };
        query.as_str().clone_into(&mut setup.query);
        publish_isolation_candidates(&views, host, setup);
    });

    let views = workspace.clone();
    ui.on_acknowledge_isolation(move |kind, id, what, accepted| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        views.update(host, |view| match what.as_str() {
            "missing" => view.isolation_accept_missing = accepted,
            "unrecorded" => view.isolation_accept_unrecorded = accepted,
            "linked" => view.isolation_sync_linked_servers = accepted,
            _ => {}
        });
    });

    let views = workspace.clone();
    ui.on_dismiss_isolation(move |kind, id| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        if let Some(setup) = views.isolation.borrow_mut().setup_mut(host)
            && setup.phase == "result"
        {
            "none".clone_into(&mut setup.phase);
            views.update(host, |view| {
                view.isolation_phase = "none".into();
                view.workflow_id = SharedString::default();
                view.workflow_title = SharedString::default();
                view.workflow_summary = SharedString::default();
                view.workflow_actions = model(Vec::new());
            });
        }
        views.update(host, |view| view.isolation_error = SharedString::default());
    });

    let sender = commands.clone();
    let state = Rc::clone(flow);
    let weak = ui.as_weak();
    ui.on_start_isolation(
        move |kind, id, accept_unrecorded, accept_missing, sync_linked_servers| {
            let Some(host) = backend::parse_mod_host(&kind, &id) else {
                return;
            };
            let mut state = state.borrow_mut();
            // Only a setup on screen can start; one that was discarded cannot.
            let Some((generation, pinned)) = state
                .setup(host)
                .filter(|setup| setup.phase == "setup")
                .map(|setup| (setup.generation, setup.pinned.iter().cloned().collect()))
            else {
                return;
            };
            let Some(request) =
                claim_isolation_change(&weak, &mut state, host, IsolationMutation::Start)
            else {
                return;
            };
            send(
                &sender,
                Command::BeginIsolation {
                    host,
                    generation,
                    request,
                    pinned,
                    accept_unrecorded,
                    accept_missing,
                    sync_linked_servers,
                },
            );
        },
    );

    let sender = commands.clone();
    let state = Rc::clone(flow);
    let weak = ui.as_weak();
    ui.on_answer_isolation(move |kind, id, answer| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let mut state = state.borrow_mut();
        let Some(generation) = answerable(&state, host) else {
            return;
        };
        let Some(request) =
            claim_isolation_change(&weak, &mut state, host, IsolationMutation::Answer)
        else {
            return;
        };
        send(
            &sender,
            Command::AnswerIsolation {
                host,
                generation,
                request,
                answer: if answer == "present" {
                    ephervex_core::RoundAnswer::Reproduced
                } else {
                    ephervex_core::RoundAnswer::DidNotReproduce
                },
            },
        );
    });

    let sender = commands.clone();
    let state = Rc::clone(flow);
    let weak = ui.as_weak();
    ui.on_undo_isolation(move |kind, id| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let mut state = state.borrow_mut();
        let Some(generation) = answerable(&state, host) else {
            return;
        };
        let Some(request) =
            claim_isolation_change(&weak, &mut state, host, IsolationMutation::Undo)
        else {
            return;
        };
        send(
            &sender,
            Command::UndoIsolation {
                host,
                generation,
                request,
            },
        );
    });

    let sender = commands.clone();
    let state = Rc::clone(flow);
    let weak = ui.as_weak();
    ui.on_finish_isolation(move |kind, id| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let mut state = state.borrow_mut();
        if state.setup(host).is_some_and(|setup| setup.unreconciled) {
            return;
        }
        let Some(request) =
            claim_isolation_change(&weak, &mut state, host, IsolationMutation::Finish)
        else {
            return;
        };
        let generation = state.generation_of(host);
        send(
            &sender,
            Command::FinishIsolation {
                host,
                generation,
                request,
            },
        );
    });

    let sender = commands.clone();
    let state = Rc::clone(flow);
    let weak = ui.as_weak();
    ui.on_retry_isolation_recovery(move |kind, id| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let mut state = state.borrow_mut();
        let Some(generation) = state
            .setup(host)
            .filter(|setup| setup.unreconciled)
            .map(|setup| setup.generation)
        else {
            return;
        };
        let Some(request) =
            claim_isolation_change(&weak, &mut state, host, IsolationMutation::Recover)
        else {
            return;
        };
        send(
            &sender,
            Command::RecoverIsolation {
                host,
                generation,
                request,
            },
        );
    });

    let views = workspace.clone();
    ui.on_discard_isolation_setup(move |kind, id| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        // A start in flight would be orphaned: its session would begin with
        // nothing left on screen to show it.
        let flow = views.isolation.borrow();
        let current = flow.setup(host).is_some() && flow.pending.is_none();
        drop(flow);
        if current {
            discard_isolation_setup(&views, host);
        }
    });

    let sender = commands.clone();
    let state = Rc::clone(flow);
    let weak = ui.as_weak();
    ui.on_cancel_isolation(move |kind, id| {
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        let mut state = state.borrow_mut();
        let Some(request) =
            claim_isolation_change(&weak, &mut state, host, IsolationMutation::Cancel)
        else {
            return;
        };
        let generation = state.generation_of(host);
        send(
            &sender,
            Command::CancelIsolation {
                host,
                generation,
                request,
            },
        );
    });

    let views = workspace.clone();
    let weak = ui.as_weak();
    ui.on_workflow_action(move |pane, kind, id, workflow, action| {
        let Some(ui) = weak.upgrade() else { return };
        route_workflow_action(&ui, &views, pane, &kind, &id, &workflow, &action);
    });
}

/// The generation a round answer or undo is sent with, or nothing when the
/// host's isolation is not loaded or its round's mods are not back yet.
fn answerable(state: &IsolationFlowState, host: InstalledHost) -> Option<u64> {
    state
        .setup(host)
        .filter(|setup| !setup.unreconciled)
        .map(|setup| setup.generation)
}

/// Sends a durable workflow's action to the feature that owns the workflow.
///
/// The workspace shows workflow actions without knowing what they do; this is
/// the one place that reads a workflow id.
fn route_workflow_action(
    ui: &AppWindow,
    workspace: &Workspace,
    pane: i32,
    kind: &str,
    id: &str,
    workflow: &str,
    action: &str,
) {
    if workflow != ISOLATION_WORKFLOW {
        return;
    }
    let (kind, id) = (SharedString::from(kind), SharedString::from(id));
    match action {
        "details" => {
            if let Some(pane) = workspace::pane_id(pane) {
                workspace.change_pane(ui, pane, |layout| {
                    layout.show_surface(pane, Surface::Isolation).is_ok()
                });
            }
        }
        "gone" | "present" => ui.invoke_answer_isolation(kind, id, action.into()),
        "undo" => ui.invoke_undo_isolation(kind, id),
        "finish" => ui.invoke_finish_isolation(kind, id),
        "dismiss" => ui.invoke_dismiss_isolation(kind, id),
        _ => {}
    }
}

/// The id isolation's durable workflow is shown under.
const ISOLATION_WORKFLOW: &str = "mod-isolation";

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
                    instance: None,
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
    host: Option<InstalledHost>,
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
    fn begin(&mut self, host: InstalledHost, plan: &backend::LocalModPlanData) {
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

/// The Instances page's list controls, and the favorite, group and icon
/// actions on hosts.
fn register_instance_list(
    ui: &AppWindow,
    commands: &Sender<Command>,
    list: &Rc<RefCell<InstanceListState>>,
    lifecycle: &Rc<RefCell<HostLifecycle>>,
) {
    attach_icon_pool(ui);
    let weak = ui.as_weak();
    let shown = Rc::clone(list);
    let stamped = Rc::clone(lifecycle);
    ui.on_instance_view_changed(move || {
        if let Some(ui) = weak.upgrade() {
            shown.borrow().render(&ui);
            stamp_host_lifecycle(&ui, &stamped.borrow());
        }
    });

    let weak = ui.as_weak();
    let ordered = Rc::clone(list);
    let stamped = Rc::clone(lifecycle);
    let sender = commands.clone();
    ui.on_choose_instance_sort(move |index| {
        let Some(sort) = usize::try_from(index)
            .ok()
            .and_then(|index| ephervex_core::InstanceSort::ALL.get(index).copied())
        else {
            return;
        };
        if let Some(ui) = weak.upgrade() {
            ordered.borrow_mut().set_sort(&ui, sort);
            stamp_host_lifecycle(&ui, &stamped.borrow());
        }
        send(&sender, Command::SetInstanceSort(sort));
    });

    let sender = commands.clone();
    ui.on_set_favorite(move |kind, id, favorite| {
        if let Some(host) = backend::parse_mod_host(&kind, &id) {
            send(&sender, Command::SetFavorite { host, favorite });
        }
    });

    let sender = commands.clone();
    ui.on_set_client_group(move |id, group| {
        let Some(client) = backend::parse_instance_id(&id) else {
            return;
        };
        let group = if group.is_empty() {
            None
        } else if let Ok(group) = group.parse() {
            Some(group)
        } else {
            return;
        };
        send(&sender, Command::SetClientGroup { client, group });
    });

    let sender = commands.clone();
    ui.on_delete_group(move |id| {
        if let Ok(group) = id.parse() {
            send(&sender, Command::DeleteGroup(group));
        }
    });

    let sender = commands.clone();
    ui.on_pick_host_icon(move |kind, id| {
        if let Some(host) = backend::parse_mod_host(&kind, &id) {
            send(&sender, Command::PickHostIcon(host));
        }
    });

    let sender = commands.clone();
    ui.on_clear_host_icon(move |kind, id| {
        if let Some(host) = backend::parse_mod_host(&kind, &id) {
            send(&sender, Command::ClearHostIcon(host));
        }
    });
}

fn register_client_actions(
    ui: &AppWindow,
    commands: &Sender<Command>,
    creations: &Rc<RefCell<CreationTracker>>,
) {
    register_promotion_actions(ui, commands);
    register_pack_link_actions(ui, commands);
    let sender = commands.clone();
    let weak = ui.as_weak();
    let submit_creations = Rc::clone(creations);
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

    register_instance_selection(ui, commands);

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
    ui.on_delete_client(move |id, files, stop_linked_servers| {
        if let Some(id) = backend::parse_instance_id(&id) {
            send(
                &sender,
                Command::DeleteClient {
                    id,
                    files,
                    stop_linked_servers,
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_delete_server(move |id, files| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::DeleteServer { id, files });
        }
    });

    let sender = commands.clone();
    ui.on_load_instance_workspace(move |id| {
        if let Some(id) = backend::parse_instance_id(&id) {
            send(&sender, Command::LoadInstanceWorkspace(id));
        }
    });
}

fn register_mod_actions(ui: &AppWindow, commands: &Sender<Command>, workspace: &Workspace) {
    register_mod_mutations(ui, commands, workspace);
    register_stand_in_answers(ui, commands, workspace);
    register_mod_browser(ui, commands, workspace);
    register_browser_install(ui, commands, workspace);
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
    let lists = workspace.clone();
    ui.on_open_mod_manager(move |kind, id, path| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(host) = backend::parse_mod_host(&kind, &id) else {
            return;
        };
        lists.plan_requests.manager_opened();
        // The modal opens on the state the list already has, then fills in.
        // Waiting for the catalog before showing anything would make an offline
        // host feel broken.
        ui.set_mod_action_host(id);
        ui.set_mod_action_host_kind(kind);
        ui.set_mod_action_path(path.clone());
        ui.set_mod_action_name(lists.mods.borrow().title(host, &path));
        ui.set_mod_action_notice(SharedString::default());
        ui.set_mod_action_selected(SharedString::default());
        ui.set_mod_action_project_url(SharedString::default());
        ui.set_mod_action_is_pack_owned(false);
        ui.set_mod_action_pack_original_version(SharedString::default());
        ui.set_mod_versions(model(Vec::new()));
        ui.set_mod_action_can_change(false);
        ui.set_mod_action_can_update(false);
        ui.set_mod_action_can_remove(false);
        ui.set_mod_action_up_to_date(false);
        ui.set_mod_action_pending(false);
        ui.set_mod_action_loading(true);
        ui.set_mod_action_requires(model(Vec::new()));
        ui.set_mod_action_required_by(model(Vec::new()));
        ui.set_mod_action_requires_note(SharedString::default());
        ui.set_mod_action_relations_refreshable(false);
        ui.set_modal_error(SharedString::default());
        ui.set_modal("mod-manage".into());
        send(
            &sender,
            Command::LoadModRelations {
                host,
                path: mod_path_key::decode(path.as_str()),
            },
        );
        send(
            &sender,
            Command::LoadModVersions {
                host,
                path: mod_path_key::decode(path.as_str()),
            },
        );
        // Last, because the worker takes these in order: both offline answers
        // are already on screen by the time this one reaches a catalog.
        send(
            &sender,
            Command::ResolveDeclaredRelations {
                host,
                path: mod_path_key::decode(path.as_str()),
            },
        );
    });
}

/// The host the open mod dialog acts on.
fn mod_action_host(ui: &AppWindow, id: &str) -> Option<InstalledHost> {
    backend::parse_mod_host(&ui.get_mod_action_host_kind(), id)
}

/// Searching a catalog, paging further into the results, and switching which
/// catalog is being searched.
///
/// Deciding what fits this instance and resolving a dependency graph both
/// happen in `ephervex-core`; this side never asks a provider anything.
fn register_mod_browser(ui: &AppWindow, commands: &Sender<Command>, workspace: &Workspace) {
    // One counter for every search request any browser ever sends, so two
    // requests that end up identical in every other way (the same instance,
    // catalog, text, and offset, asked for twice because the user switched
    // catalog and back) still mint distinct ids.
    let sender = commands.clone();
    let handle = ui.as_weak();
    let browsers = workspace.clone();
    ui.on_search_mods(move |kind, id, provider, query| {
        let (Some(ui), Some(host)) = (handle.upgrade(), backend::parse_mod_host(&kind, &id)) else {
            return;
        };
        let command = browsers.mods.borrow_mut().search(
            host,
            &provider,
            &query,
            &browsers.browser.search_requests,
            ui.get_mod_page_size(),
        );
        mod_views::publish(&browsers, &browsers.mods.borrow(), host);
        mod_views::send_if(&sender, command);
    });

    let sender = commands.clone();
    let handle = ui.as_weak();
    let browsers = workspace.clone();
    ui.on_load_more_mods(move |kind, id, provider, query, offset| {
        let (Some(ui), Some(host)) = (handle.upgrade(), backend::parse_mod_host(&kind, &id)) else {
            return;
        };
        let command = browsers.mods.borrow_mut().load_more(
            host,
            &provider,
            &query,
            offset,
            &browsers.browser.search_requests,
            ui.get_mod_page_size(),
        );
        mod_views::publish(&browsers, &browsers.mods.borrow(), host);
        mod_views::send_if(&sender, command);
    });

    let browsers = workspace.clone();
    ui.on_switch_mod_catalog(move |kind, id, provider| {
        if let Some(host) = backend::parse_mod_host(&kind, &id)
            && browsers.mods.borrow_mut().switch_catalog(host, &provider)
        {
            mod_views::publish(&browsers, &browsers.mods.borrow(), host);
        }
    });

    let sender = commands.clone();
    let handle = ui.as_weak();
    let browsers = workspace.clone();
    ui.on_set_mod_include_incompatible(move |kind, id, on| {
        let (Some(ui), Some(host)) = (handle.upgrade(), backend::parse_mod_host(&kind, &id)) else {
            return;
        };
        let asked_again = browsers
            .mods
            .borrow_mut()
            .set_include_incompatible(host, on);
        let command = if asked_again {
            browsers.mods.borrow_mut().repeat_search(
                host,
                &browsers.browser.search_requests,
                ui.get_mod_page_size(),
            )
        } else {
            None
        };
        mod_views::publish(&browsers, &browsers.mods.borrow(), host);
        mod_views::send_if(&sender, command);
    });

    let lists = workspace.clone();
    ui.on_filter_mods(move |kind, id, query| {
        if let Some(host) = backend::parse_mod_host(&kind, &id)
            && lists.mods.borrow_mut().filter(host, &query)
        {
            mod_views::publish(&lists, &lists.mods.borrow(), host);
        }
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
fn register_browser_install(ui: &AppWindow, commands: &Sender<Command>, workspace: &Workspace) {
    let sender = commands.clone();
    ui.on_install_server_mod(
        move |server, provider, project, version, confirmed_support| {
            if let Some(command) = server_install_command(
                &server,
                &provider,
                &project,
                &version,
                confirmed_support.iter().map(|id| id.to_string()).collect(),
                true,
            ) {
                send(&sender, command);
            }
        },
    );

    let sender = commands.clone();
    let install_workspace = workspace.clone();
    let install_queue = Rc::clone(&workspace.browser);
    ui.on_install_from_browser(move |kind, instance, provider, project| {
        if backend::parse_mod_host(&kind, &instance).is_none()
            || ModProvider::parse(&provider).is_none()
        {
            return;
        }
        let request = BrowserInstall {
            server: kind == "server",
            instance: instance.to_string(),
            provider: provider.to_string(),
            project: project.to_string(),
        };
        let start = install_queue.installs.borrow_mut().enqueue(request);
        if let Some(request) = start {
            start_browser_install(&sender, &request);
        }
        mark_queued(&install_workspace);
    });

    // Diagnostics names a missing mod, not a release, so its plan is always
    // shown before anything is installed.
    let sender = commands.clone();
    let install_workspace = workspace.clone();
    let install_queue = Rc::clone(&workspace.browser);
    ui.on_install_from_diagnostics(move |kind, instance, provider, project| {
        if backend::parse_mod_host(&kind, &instance).is_none()
            || ModProvider::parse(&provider).is_none()
        {
            return;
        }
        let request = BrowserInstall {
            server: kind == "server",
            instance: instance.to_string(),
            provider: provider.to_string(),
            project: project.to_string(),
        };
        let start = install_queue
            .installs
            .borrow_mut()
            .enqueue_confirmed(request);
        if let Some(request) = start {
            start_browser_install(&sender, &request);
        }
        mark_queued(&install_workspace);
    });

    let sender = commands.clone();
    let abandon_workspace = workspace.clone();
    ui.on_abandon_browser_install(move || {
        let next = abandon_workspace
            .browser
            .installs
            .borrow_mut()
            .finish_active();
        advance_browser_install(&sender, &abandon_workspace, next);
    });
}

/// Asks for one queued install's plan.
///
/// The request is identified by the queue alone. Whatever dialog is open belongs
/// to the user, so nothing here reads or writes its host or its selections.
fn start_browser_install(commands: &Sender<Command>, request: &BrowserInstall) {
    let Some(catalog) = ModProvider::parse(&request.provider) else {
        return;
    };
    if request.server {
        if let Some(server) = backend::parse_server_id(&request.instance) {
            send(
                commands,
                Command::PlanServerMod {
                    server,
                    provider: catalog,
                    project: request.project.clone(),
                },
            );
        }
        return;
    }
    let Some(instance) = backend::parse_instance_id(&request.instance) else {
        return;
    };
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
    commands: &Sender<Command>,
    workspace: &Workspace,
    next: Option<BrowserInstall>,
) {
    if let Some(next) = next {
        start_browser_install(commands, &next);
    }
    mark_queued(workspace);
}

/// Marks, in every shown client's browser, the results whose install is
/// queued or under way.
fn mark_queued(workspace: &Workspace) {
    let marked = workspace
        .mods
        .borrow_mut()
        .mark_queued(&workspace.browser.installs.borrow());
    for host in marked {
        mod_views::publish(workspace, &workspace.mods.borrow(), host);
    }
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
fn register_mod_mutations(ui: &AppWindow, commands: &Sender<Command>, workspace: &Workspace) {
    register_loader_change(ui, commands);

    let sender = commands.clone();
    let handle = ui.as_weak();
    ui.on_change_mod_version(move |id, path, version| {
        let Some(ui) = handle.upgrade() else { return };
        if let Some(host) = mod_action_host(&ui, &id) {
            send(
                &sender,
                Command::ChangeModVersion {
                    host,
                    path: mod_path_key::decode(path.as_str()),
                    version: version.to_string(),
                    servers: chosen_linked_servers(&ui),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_cancel_mod_acquisition(move |id| {
        if let Ok(id) = id.parse() {
            send(&sender, Command::CancelModAcquisition { id });
        }
    });

    let sender = commands.clone();
    let handle = ui.as_weak();
    let plans = Rc::clone(&workspace.plan_requests);
    ui.on_plan_mod_apply(move |id, path, version| {
        let Some(ui) = handle.upgrade() else { return };
        if let Some(host) = mod_action_host(&ui, &id) {
            let request = plans.ask(PlanKind::Apply, ui.get_modal().as_str());
            send(
                &sender,
                Command::PlanModApply {
                    request,
                    host,
                    path: mod_path_key::decode(path.as_str()),
                    version: version.to_string(),
                },
            );
        }
    });

    let sender = commands.clone();
    let handle = ui.as_weak();
    let plans = Rc::clone(&workspace.plan_requests);
    ui.on_plan_mod_removal(move |kind, id, path| {
        let Some(ui) = handle.upgrade() else { return };
        ui.set_mod_action_host(id.clone());
        ui.set_mod_action_host_kind(kind.clone());
        if let Some(host) = backend::parse_mod_host(&kind, &id) {
            let request = plans.ask(PlanKind::Removal, ui.get_modal().as_str());
            send(
                &sender,
                Command::PlanModRemoval {
                    request,
                    host,
                    path: mod_path_key::decode(path.as_str()),
                },
            );
        }
    });

    let sender = commands.clone();
    let handle = ui.as_weak();
    ui.on_remove_mod(move |id, path| {
        let Some(ui) = handle.upgrade() else { return };
        if let Some(host) = mod_action_host(&ui, &id) {
            send(
                &sender,
                Command::RemoveMod {
                    host,
                    path: mod_path_key::decode(path.as_str()),
                    servers: chosen_linked_servers(&ui),
                },
            );
        }
    });

    let sender = commands.clone();
    let handle = ui.as_weak();
    ui.on_install_mod(move |instance, provider, project, version| {
        let Some(ui) = handle.upgrade() else { return };
        if let Some(command) = install_command(
            &instance,
            &provider,
            &project,
            &version,
            chosen_linked_servers(&ui),
            true,
        ) {
            send(&sender, command);
        }
    });
}

/// Answers to the question whether an installed jar may stand in for a
/// dependency.
fn register_stand_in_answers(ui: &AppWindow, commands: &Sender<Command>, workspace: &Workspace) {
    let sender = commands.clone();
    let handle = ui.as_weak();
    let acquisitions = Rc::clone(&workspace.acquisition_prompts);
    let stand_ins = Rc::clone(&workspace.stand_in_prompts);
    ui.on_answer_mod_stand_in(move |id, accepted| {
        let Some(ui) = handle.upgrade() else { return };
        if let Ok(id) = id.parse() {
            send(&sender, Command::AnswerModStandIn { id, accepted });
        }
        ui.set_mod_stand_in_id(SharedString::default());
        if ui.get_modal() == "mod-stand-in" {
            ui.set_modal(SharedString::default());
            ui.set_modal_error(SharedString::default());
        }
        show_next_prompt(&ui, &acquisitions, &stand_ins);
    });
}

/// The install of one exact release the confirmation showed, from the catalog
/// the confirmation was for. `confirmed` says a dialog asked for it.
fn install_command(
    instance: &str,
    provider: &str,
    project: &str,
    version: &str,
    servers: Vec<ephervex_model::ServerId>,
    confirmed: bool,
) -> Option<Command> {
    Some(Command::InstallMod {
        instance: backend::parse_instance_id(instance)?,
        provider: ModProvider::parse(provider)?,
        project: project.to_owned(),
        version: version.to_owned(),
        servers,
        confirmed,
    })
}

/// The install of one exact release onto a server. `confirmed_support` names
/// the projects whose undeclared server support the user agreed to.
fn server_install_command(
    server: &str,
    provider: &str,
    project: &str,
    version: &str,
    confirmed_support: Vec<String>,
    confirmed: bool,
) -> Option<Command> {
    Some(Command::InstallServerMod {
        server: backend::parse_server_id(server)?,
        provider: ModProvider::parse(provider)?,
        project: project.to_owned(),
        version: version.to_owned(),
        confirmed_support,
        confirmed,
    })
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
    ui.on_open_loader_change(move |id| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(row) = ui
            .get_host_views()
            .iter()
            .find(|view| view.kind == "client" && view.id == id)
            .map(|view| view.client)
        else {
            return;
        };
        let Some(instance) = backend::parse_instance_id(&id) else {
            return;
        };
        ui.set_mod_action_host(id);
        // Vanilla has no build to change; the control that opens this is
        // hidden for it, and this is the second gate.
        let Some(loader) = loader_kind_of(&row.loader_kind) else {
            return;
        };
        ui.set_loader_change_current(row.loader.clone());
        ui.set_loader_change_current_build(
            row.loader
                .as_str()
                .strip_prefix(&format!("{loader} "))
                .unwrap_or_default()
                .into(),
        );
        ui.set_loader_change_loader(loader.to_string().into());
        ui.set_loader_change_minecraft(row.minecraft.clone());
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
                instance: Some(instance),
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

fn register_instance_selection(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    // What this handler last acted on. The markup assigns `selected-instance-id`
    // before calling in, so the property cannot tell a new selection from a
    // repeat of the current one.
    let selected = Rc::new(RefCell::new(String::new()));
    ui.on_select_instance(move |id| {
        let repeat = *selected.borrow() == id.as_str();
        if !repeat {
            id.as_str().clone_into(&mut selected.borrow_mut());
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
                path: mod_path_key::decode(path.as_str()),
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
                path: mod_path_key::decode(path.as_str()),
                enabled,
            },
        );
    });

    register_dependency_actions(ui, commands);
}

/// Answering the dialog that says what a mod change affects.
///
/// Its own registration because it is a whole conversation rather than one
/// more toggle: what the dialog is holding decides which question was asked.
fn register_dependency_actions(ui: &AppWindow, commands: &Sender<Command>) {
    // Applying what the dialog asked about. The dialog carries the direction,
    // so one callback serves both questions.
    let sender = commands.clone();
    let handle = ui.as_weak();
    ui.on_apply_mod_impact(move |cascade| {
        let Some(ui) = handle.upgrade() else {
            return;
        };
        let Some(host) =
            backend::parse_mod_host(&ui.get_mod_impact_kind(), &ui.get_mod_impact_host())
        else {
            return;
        };
        if ui.get_mod_impact_remember() {
            send(&sender, Command::SaveDependencyWarning(false));
        }
        let target = mod_path_key::decode(ui.get_mod_impact_path().as_str());
        if ui.get_mod_impact_deleting() {
            // The jars that stop loading go too when the user agreed to it.
            // Taking only the one named would leave the others installed and
            // unable to load, which is the state the dialog exists to prevent.
            let mut paths = Vec::new();
            if cascade {
                paths.extend(
                    ui.get_mod_impact_breaks()
                        .iter()
                        .map(|row| mod_path_key::decode(row.logical_path.as_str())),
                );
            }
            paths.push(target.clone());
            send(&sender, Command::DeleteModFiles { host, paths });
        } else {
            let enabling = ui.get_mod_impact_enabling();
            let mut changes: Vec<(PathBuf, bool)> = Vec::new();
            if cascade {
                let others = if enabling {
                    ui.get_mod_requires_disabled()
                } else {
                    ui.get_mod_impact_breaks()
                };
                changes.extend(
                    others
                        .iter()
                        .map(|row| (mod_path_key::decode(row.logical_path.as_str()), enabling)),
                );
            }
            changes.push((target, enabling));
            send(&sender, Command::SetModsEnabled { host, changes });
        }
        ui.set_modal(SharedString::new());
        ui.set_modal_error(SharedString::new());
    });

    // Asked from two places: the impact dialog, which closes itself, and a
    // mod's own page, which stays open and is handed the new answer.
    let sender = commands.clone();
    ui.on_refresh_mod_dependencies(move |kind, host, path| {
        let Some(host) = backend::parse_mod_host(&kind, &host) else {
            return;
        };
        send(
            &sender,
            Command::RefreshModDependencies {
                host,
                reload: (!path.is_empty()).then(|| mod_path_key::decode(path.as_str())),
            },
        );
    });
}

/// Shows what turning a mod off, or deleting its jar, would do to the rest.
fn apply_mod_impact(ui: &AppWindow, impact: &ModImpactPresentation) {
    if !make_room_for_dialog(ui) {
        return;
    }
    ui.set_mod_impact_kind(impact.host_kind.as_str().into());
    ui.set_mod_impact_host(impact.host_id.as_str().into());
    ui.set_mod_impact_path(impact.path.as_str().into());
    ui.set_mod_impact_name(impact.name.as_str().into());
    ui.set_mod_impact_deleting(impact.deleting);
    ui.set_mod_impact_enabling(false);
    ui.set_mod_impact_breaks(impact_rows(&impact.breaks));
    ui.set_mod_impact_degrades(impact_rows(&impact.degrades));
    ui.set_mod_impact_unknown(impact_rows(&impact.unknown));
    ui.set_mod_impact_unidentifiable(impact_rows(&impact.unidentifiable));
    // The other question's lists are cleared, so one dialog never shows half
    // an answer left over from the other.
    ui.set_mod_requires_disabled(impact_rows(&[]));
    ui.set_mod_requires_absent(impact_rows(&[]));
    ui.set_mod_requires_incompatible(impact_rows(&[]));
    ui.set_mod_impact_cascade(impact.has_cascade());
    ui.set_mod_impact_remember(false);
    ui.set_modal_error(SharedString::new());
    ui.set_modal("mod-impact".into());
}

/// Shows what turning a mod on needs first.
fn apply_mod_requirements(ui: &AppWindow, needs: &ModRequirementPresentation) {
    if !make_room_for_dialog(ui) {
        return;
    }
    ui.set_mod_impact_kind(needs.host_kind.as_str().into());
    ui.set_mod_impact_host(needs.host_id.as_str().into());
    ui.set_mod_impact_path(needs.path.as_str().into());
    ui.set_mod_impact_name(needs.name.as_str().into());
    ui.set_mod_impact_deleting(false);
    ui.set_mod_impact_enabling(true);
    ui.set_mod_impact_breaks(impact_rows(&[]));
    ui.set_mod_impact_degrades(impact_rows(&[]));
    ui.set_mod_impact_unknown(impact_rows(&[]));
    ui.set_mod_impact_unidentifiable(impact_rows(&[]));
    ui.set_mod_requires_disabled(impact_rows(&needs.disabled));
    ui.set_mod_requires_absent(impact_rows(&needs.absent));
    ui.set_mod_requires_incompatible(impact_rows(&needs.incompatible));
    ui.set_mod_impact_cascade(needs.has_cascade());
    ui.set_mod_impact_remember(false);
    ui.set_modal_error(SharedString::new());
    ui.set_modal("mod-impact".into());
}

fn impact_rows(rows: &[crate::presentation::ImpactRowPresentation]) -> ModelRc<ImpactRow> {
    ModelRc::new(VecModel::from(
        rows.iter()
            .map(|row| ImpactRow {
                name: row.name.as_str().into(),
                logical_path: row.logical_path.as_str().into(),
                distance: row.distance,
            })
            .collect::<Vec<_>>(),
    ))
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

    let weak = ui.as_weak();
    let sender = commands.clone();
    ui.on_deliver_pack_link(move |url| {
        let Some(ui) = weak.upgrade() else { return };
        send(
            &sender,
            Command::OpenPackUrl {
                generation: begin_pack_link_request(&ui),
                url: url.to_string(),
                host: None,
                consent: None,
            },
        );
    });

    let weak = ui.as_weak();
    let sender = commands.clone();
    ui.on_cancel_pack_link(move || {
        if let Some(ui) = weak.upgrade() {
            cancel_pack_link_download(&ui, &sender);
        }
    });

    let weak = ui.as_weak();
    let sender = commands.clone();
    ui.on_open_pack_link(move |url, host, remember| {
        let Some(ui) = weak.upgrade() else { return };
        let consent = if remember {
            ephervex_core::HostConsent::Remember
        } else {
            ephervex_core::HostConsent::JustOnce
        };
        send(
            &sender,
            Command::OpenPackUrl {
                generation: begin_pack_link_request(&ui),
                url: url.to_string(),
                host: Some(host.to_string()),
                consent: Some(consent),
            },
        );
    });
}

/// Stops the modpack link download the window is showing, if there is one.
///
/// Clearing the flag is what keeps its archive out of the import: a download
/// that finishes anyway is no longer wanted, and is dropped when it arrives.
fn cancel_pack_link_download(ui: &AppWindow, commands: &Sender<Command>) {
    if ui.get_pack_link_downloading() {
        ui.set_pack_link_downloading(false);
        send(
            commands,
            Command::CancelPackUrl {
                generation: ui.get_pack_link_generation(),
            },
        );
    }
}

/// Starts following a modpack link request and returns its number.
///
/// The window numbers its requests itself, so it knows which one an event or a
/// Cancel is about even before the backend has answered the first command.
fn begin_pack_link_request(ui: &AppWindow) -> i32 {
    let generation = ui.get_pack_link_generation().saturating_add(1);
    ui.set_pack_link_generation(generation);
    ui.set_pack_link_downloading(true);
    generation
}

/// Whether an event is about the request the window is still waiting on,
/// rather than one that was replaced, answered or canceled since.
fn pack_link_request_pending(ui: &AppWindow, generation: i32) -> bool {
    ui.get_pack_link_downloading() && ui.get_pack_link_generation() == generation
}

/// Hands the window a modpack link, as a desktop scheme handler delivers one.
///
/// Takes the window rather than the controller because both callers have one:
/// `main` for the link this process was started for, and the single-instance
/// listener for a link a later launch handed over.
pub fn open_pack_link(ui: &AppWindow, url: &str) {
    ui.invoke_deliver_pack_link(url.into());
}

/// Puts this build's third-party license inventory on the "About & Licenses"
/// screen.
///
/// Compiled-in build data, so it is set once at startup and never refreshed:
/// what Ephervex is built on cannot change while it runs.
pub fn show_third_party_licenses(ui: &AppWindow) {
    let licenses = ThirdPartyLicenses::bundled();
    let rows: Vec<LicenseRow> = licenses
        .packages
        .iter()
        .map(|package| LicenseRow {
            name: package.name.as_str().into(),
            version: package.version.as_str().into(),
            license: package.license.as_str().into(),
            text: package.text.as_str().into(),
        })
        .collect();
    ui.set_third_party_packages(ModelRc::new(VecModel::from(rows)));
    ui.set_third_party_summary(licenses.summary.as_str().into());
    ui.set_third_party_error(licenses.error.as_deref().unwrap_or_default().into());
    ui.set_third_party_open(-1);
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
    let handle = ui.as_weak();
    ui.on_delete_hand_placed_mod(move |kind, host, path| {
        let Some(host) = backend::parse_mod_host(&kind, &host) else {
            return;
        };
        let path = mod_path_key::decode(path.as_str());
        // Deleting a jar takes it out of the loader's view exactly as turning
        // it off does, so it asks the same question first.
        if let Some(ui) = handle.upgrade()
            && ui.get_modal() == "delete-mod"
        {
            send(
                &sender,
                Command::QueryDeleteImpact {
                    host,
                    path,
                    name: ui.get_mod_action_name().to_string(),
                },
            );
            return;
        }
        send(&sender, Command::DeleteHandPlacedMod { host, path });
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
            "group" => {
                if let Ok(group) = id.parse() {
                    send(&sender, Command::RenameGroup { group, name });
                }
            }
            "new-group" => send(
                &sender,
                Command::CreateGroup {
                    name,
                    assign_to: backend::parse_instance_id(&id),
                },
            ),
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

/// The workspace's layout together with the host state every pane reads.
///
/// Cloned into each callback that changes what a pane shows, because showing a
/// host means rendering it with its current restart stage.
#[derive(Clone)]
struct Workspace {
    state: Rc<RefCell<WorkspaceState>>,
    lifecycle: Rc<RefCell<HostLifecycle>>,
    logs: Rc<RefCell<LogViews>>,
    /// Each shown host's mod list and catalog browser. Kept for exactly the
    /// hosts the workspace shows.
    mods: Rc<RefCell<ModViews>>,
    /// The install queue and search ids every browser shares.
    browser: Rc<ModBrowser>,
    /// Each shown host's isolation setup, and the one isolation change in flight.
    isolation: Rc<RefCell<IsolationFlowState>>,
    /// Each shown host's Diagnostics surface, and the one diagnosis or fix
    /// in flight.
    diagnosis: Rc<RefCell<DiagnosisFlowState>>,
    /// Manual-download prompts waiting behind the one on screen.
    acquisition_prompts: Rc<RefCell<std::collections::VecDeque<backend::ModAcquisitionData>>>,
    /// Questions whether an installed jar may stand in for a dependency,
    /// waiting behind the prompt on screen.
    stand_in_prompts: Rc<RefCell<std::collections::VecDeque<backend::ModStandInData>>>,
    /// The mod plans asked for and not yet shown.
    plan_requests: Rc<ModPlanRequests>,
    /// Each shown server's Properties form, with its unsaved edits.
    properties: Rc<RefCell<server_property_form::ServerPropertyForms>>,
    /// What a surface asks the worker for when a pane opens it.
    commands: Sender<Command>,
}

impl Workspace {
    /// Renders the workspace and gives a pane what its new host and surface need.
    fn present(&self, ui: &AppWindow, change: Option<PaneChange>) {
        let panes = self
            .state
            .borrow()
            .layout()
            .map_or_else(Vec::new, |layout| {
                layout
                    .panes()
                    .iter()
                    .map(|placed| (placed.pane.id(), placed.pane.host(), placed.pane.surface()))
                    .collect()
            });
        let mut logs = self.logs.borrow_mut();
        logs.sync(&panes);
        let entered = self
            .state
            .borrow_mut()
            .render(ui, &self.lifecycle.borrow(), &logs);
        drop(logs);
        self.follow_hosts(ui, &entered);
        let state = self.state.borrow();
        // A single pane is the workspace of its host's page; a split belongs to
        // the page it was opened from.
        if let Some((host, _)) = state.single_content() {
            ui.set_page(match host {
                InstalledHost::Instance(_) => INSTANCES_PAGE,
                InstalledHost::Server(_) => SERVERS_PAGE,
            });
        }
        drop(state);
        if let Some(change) = change {
            apply_pane_change(ui, self, change);
        }
    }

    /// Redraws every host view from the newest rows, as a snapshot requires.
    fn refresh(&self, ui: &AppWindow) {
        let entered =
            self.state
                .borrow_mut()
                .render(ui, &self.lifecycle.borrow(), &self.logs.borrow());
        self.follow_hosts(ui, &entered);
    }

    /// Lets each host's feature state follow the workspace's hosts, and asks
    /// for everything the hosts that just entered show.
    fn follow_hosts(&self, ui: &AppWindow, entered: &[InstalledHost]) {
        let shown = self.state.borrow().shown();
        // A client opens its browser on the preferred catalog. A name this
        // build cannot resolve leaves it on the default rather than searching
        // one catalog while the setting names another.
        let preferred = ui.get_default_mod_catalog();
        let default = ModProvider::Modrinth.as_str();
        let catalog = presentation::catalog_to_open_on(&preferred, default).unwrap_or(default);
        self.mods.borrow_mut().follow(&shown, catalog);
        self.isolation.borrow_mut().follow(&shown);
        self.diagnosis.borrow_mut().follow(&shown);
        for &host in entered {
            mod_views::publish(self, &self.mods.borrow(), host);
            load_host(ui, host);
            java_runtime::load(&self.commands, host, false);
        }
    }

    /// Changes what a feature reported about a host, in every pane showing it.
    fn update(&self, host: InstalledHost, change: impl FnOnce(&mut HostView)) -> bool {
        self.state.borrow_mut().update(host, change)
    }

    /// Clears what a host's last run left behind once it starts again. A new
    /// failure brings its own notices.
    fn forget_last_run(&self, host: InstalledHost) {
        self.forget_last_run_from(host, backend::run_clock());
    }

    /// [`Self::forget_last_run`] for a start the backend began when the run
    /// clock read `floor`.
    fn forget_last_run_from(&self, host: InstalledHost, floor: u64) {
        self.diagnosis.borrow_mut().note_client_start(host, floor);
        diagnosis_flow::forget_run(self, host);
        self.update(host, |view| {
            view.crash_report = SharedString::default();
            view.crash_failure = SharedString::default();
            view.crash_client_code_on_server = false;
            view.crash_suggested = 0;
        });
    }

    fn open(&self, ui: &AppWindow, host: InstalledHost, surface: Surface) {
        let change = self.state.borrow_mut().open(host, surface);
        self.present(ui, Some(change));
    }

    fn close(&self, ui: &AppWindow) {
        self.state.borrow_mut().close();
        self.present(ui, None);
    }

    fn change_pane(
        &self,
        ui: &AppWindow,
        pane: PaneId,
        change: impl FnOnce(&mut WorkspaceLayout) -> bool,
    ) {
        let change = self.state.borrow_mut().change_pane(pane, change);
        if change.is_some() {
            self.present(ui, change);
        }
    }

    /// Puts a started server's console on screen.
    ///
    /// A single workspace follows the server from wherever it was started, so
    /// Back returns to the client that started it. A split is the user's own
    /// arrangement and is left as it is.
    fn show_console(&self, ui: &AppWindow, server: InstalledHost) {
        let content = self.state.borrow().single_content();
        let layout_open = self.state.borrow().layout().is_some();
        match content {
            Some((current, _)) => {
                let Some(pane) = self.single_pane() else {
                    return;
                };
                self.change_pane(ui, pane, |layout| {
                    if current == server {
                        layout.show_surface(pane, Surface::Console).is_ok()
                    } else {
                        layout.follow(pane, server, Surface::Console).is_ok()
                    }
                });
            }
            None if layout_open => {}
            None => self.open(ui, server, Surface::Console),
        }
    }

    fn single_pane(&self) -> Option<PaneId> {
        let state = self.state.borrow();
        let panes = state.layout()?.panes();
        match panes.as_slice() {
            [only] => Some(only.pane.id()),
            _ => None,
        }
    }

    /// Starts every console and log viewer on `server` over with its new run;
    /// a console opened afterwards reads the latest run from its beginning.
    fn reopen_server_console(&self, server: ephervex_model::ServerId) {
        self.logs
            .borrow_mut()
            .reopen(&LiveLogSource::Server(server));
    }

    /// How a fresh Launch or Start of `host` goes, claiming a pair start
    /// when its pair starts together.
    fn claim_start(&self, ui: &AppWindow, host: InstalledHost) -> HostStart {
        let mut lifecycle = self.lifecycle.borrow_mut();
        if lifecycle.pair_starting(host) {
            return HostStart::Refused(StartRefusal::PairStarting);
        }
        let Some((client, server)) = self.state.borrow().pair_starting_together(host) else {
            self.forget_last_run(host);
            return HostStart::Alone;
        };
        let Some(operation) = lifecycle.begin_pair_start(client, server) else {
            return HostStart::Refused(StartRefusal::PairStarting);
        };
        let mut starts_server = false;
        if let InstalledHost::Server(id) = server {
            let restarting = !lifecycle.stage_of(server).is_empty();
            match lifecycle.servers.claim_linked(id, restarting) {
                Ok(claim) => starts_server = claim == LinkedClaim::Starting,
                Err(refusal) => {
                    lifecycle.settle_pair_start(operation, true);
                    return HostStart::Refused(StartRefusal::Server(refusal));
                }
            }
        }
        self.forget_last_run(client);
        self.forget_last_run(server);
        stamp_host_lifecycle(ui, &lifecycle);
        HostStart::Paired {
            operation,
            client,
            server,
            starts_server,
        }
    }

    /// Shows a pane's paired host, reusing a pane that shows it or adding one
    /// where the layout has room. No pane already open is changed, and a
    /// refusal is said rather than made room for.
    ///
    /// `asked` is whether the user asked for exactly this, which is when a
    /// host already on screen is worth saying so and a refusal is an error.
    fn reveal_pair(
        &self,
        ui: &AppWindow,
        from: Option<PaneId>,
        host: InstalledHost,
        surface: Surface,
        asked: bool,
    ) {
        let Some(revealed) = self.state.borrow_mut().reveal(host, surface, from) else {
            return;
        };
        let message = match revealed {
            Ok(Revealed::Added(_)) => {
                self.present(
                    ui,
                    Some(PaneChange {
                        before: None,
                        after: Some((host, surface)),
                    }),
                );
                return;
            }
            Ok(Revealed::Shown(_)) if asked => "gui-pair-already-shown",
            Ok(Revealed::Shown(_)) => return,
            Err(_) => "gui-pair-no-room-for",
        };
        let name = self.state.borrow().host_name(host);
        ui.invoke_present_toast(
            lookup_args(message, [("value", Arg::from(name))]).into(),
            asked && message == "gui-pair-no-room-for",
        );
    }

    /// Puts both sides of a pair on screen as they start together: the
    /// workspace opens on the client when it is closed, and a side missing
    /// from it is added beside the other where the layout has room, the
    /// server on its console.
    fn show_pair(&self, ui: &AppWindow, client: InstalledHost, server: InstalledHost) {
        if self.state.borrow().layout().is_none() {
            self.open(ui, client, Surface::DEFAULT);
        }
        for (host, beside, surface) in [
            (server, client, Surface::Console),
            (client, server, Surface::DEFAULT),
        ] {
            let from = self.state.borrow().first_pane_showing(beside);
            self.reveal_pair(ui, from, host, surface, false);
        }
    }

    /// Brings a surface of `host` forward when the workspace is that host alone.
    fn reveal_surface(&self, ui: &AppWindow, host: InstalledHost, surface: Surface) {
        let Some((current, _)) = self.state.borrow().single_content() else {
            return;
        };
        if current != host {
            return;
        }
        if let Some(pane) = self.single_pane() {
            self.change_pane(ui, pane, |layout| {
                layout.show_surface(pane, surface).is_ok()
            });
        }
    }
}

/// Loads what a pane's new host or surface shows.
fn apply_pane_change(ui: &AppWindow, workspace: &Workspace, change: PaneChange) {
    let PaneChange { before, after } = change;
    if before == after {
        return;
    }
    let Some((host, surface)) = after else {
        return;
    };
    let id = SharedString::from(host.id());
    match surface {
        Surface::Snapshots => ui.invoke_load_snapshots(host.kind().into(), id),
        Surface::Packs => ui.invoke_load_pack_content(host.kind().into(), id),
        Surface::Diagnostics => diagnosis_flow::open(workspace, &workspace.commands, host),
        Surface::Mods if matches!(host, InstalledHost::Server(_)) => {
            ui.invoke_load_server_mods(id);
        }
        _ => {}
    }
}

/// Asks for everything a host's workspace shows, once, as it enters the workspace.
fn load_host(ui: &AppWindow, host: InstalledHost) {
    let id = SharedString::from(host.id());
    match host {
        InstalledHost::Instance(_) => ui.invoke_load_instance_workspace(id),
        InstalledHost::Server(_) => ui.invoke_load_server_workspace(id),
    }
}

/// Opening, leaving, and moving around the host workspace.
fn register_workspace(ui: &AppWindow, workspace: &Workspace) {
    workspace.state.borrow().attach(ui);

    let weak = ui.as_weak();
    let searched = workspace.clone();
    ui.on_pane_host_query_changed(move || {
        if let Some(ui) = weak.upgrade() {
            searched.refresh(&ui);
        }
    });

    let weak = ui.as_weak();
    let open = workspace.clone();
    ui.on_open_workspace(move |kind, id, surface| {
        let (Some(ui), Some(host)) = (weak.upgrade(), workspace::parse_host(&kind, &id)) else {
            return;
        };
        open.open(
            &ui,
            host,
            Surface::parse(&surface).unwrap_or(Surface::DEFAULT),
        );
    });

    let weak = ui.as_weak();
    let close = workspace.clone();
    ui.on_close_workspace(move || {
        if let Some(ui) = weak.upgrade() {
            close.close(&ui);
        }
    });

    let weak = ui.as_weak();
    let back = workspace.clone();
    ui.on_workspace_back(move || {
        let Some(ui) = weak.upgrade() else { return };
        let returned = back.single_pane().and_then(|pane| {
            back.state
                .borrow_mut()
                .change_pane(pane, WorkspaceLayout::go_back)
        });
        match returned {
            Some(change) => back.present(&ui, Some(change)),
            None => back.close(&ui),
        }
    });

    let weak = ui.as_weak();
    let surfaces = workspace.clone();
    ui.on_show_pane_surface(move |pane, surface| {
        let (Some(ui), Some(pane), Some(surface)) = (
            weak.upgrade(),
            workspace::pane_id(pane),
            Surface::parse(&surface),
        ) else {
            return;
        };
        surfaces.change_pane(&ui, pane, |layout| {
            layout.show_surface(pane, surface).is_ok()
        });
    });

    let weak = ui.as_weak();
    let follow = workspace.clone();
    ui.on_follow_host(move |pane, kind, id, surface| {
        let (Some(ui), Some(pane), Some(host)) = (
            weak.upgrade(),
            workspace::pane_id(pane),
            workspace::parse_host(&kind, &id),
        ) else {
            return;
        };
        let surface = Surface::parse(&surface).unwrap_or(Surface::DEFAULT);
        follow.change_pane(&ui, pane, |layout| {
            layout.follow(pane, host, surface).is_ok()
        });
    });

    register_pane_layout(ui, workspace);
    register_linked_pair(ui, workspace);
}

/// Pairing a client with a linked server, showing a pane's paired host, and
/// Launch & Connect, which shows both sides as it starts them.
fn register_linked_pair(ui: &AppWindow, workspace: &Workspace) {
    let sender = workspace.commands.clone();
    ui.on_set_linked_pair(move |client, server, start_together| {
        if let (Some(client), Some(server)) = (
            backend::parse_instance_id(&client),
            backend::parse_server_id(&server),
        ) {
            send(
                &sender,
                Command::SetLinkedPair {
                    client,
                    server,
                    start_together,
                },
            );
        }
    });

    let sender = workspace.commands.clone();
    ui.on_clear_linked_pair(move |client| {
        if let Some(client) = backend::parse_instance_id(&client) {
            send(&sender, Command::ClearLinkedPair(client));
        }
    });

    let weak = ui.as_weak();
    let reveal = workspace.clone();
    ui.on_show_linked_pair(move |pane, kind, id| {
        let (Some(ui), Some(pane), Some(host)) = (
            weak.upgrade(),
            workspace::pane_id(pane),
            workspace::parse_host(&kind, &id),
        ) else {
            return;
        };
        reveal.reveal_pair(&ui, Some(pane), host, Surface::DEFAULT, true);
    });

    let weak = ui.as_weak();
    let connect = workspace.clone();
    ui.on_launch_and_connect(move |client, server| {
        let (Some(ui), Some(client), Some(server)) = (
            weak.upgrade(),
            backend::parse_instance_id(&client),
            backend::parse_server_id(&server),
        ) else {
            return;
        };
        let (operation, claim) = {
            let mut lifecycle = connect.lifecycle.borrow_mut();
            let host = InstalledHost::Server(server);
            let restarting = !lifecycle.stage_of(host).is_empty();
            let Some(operation) = lifecycle.begin_pair_start(InstalledHost::Instance(client), host)
            else {
                ui.invoke_present_toast(StartRefusal::PairStarting.message().into(), true);
                return;
            };
            let claim = match lifecycle.servers.claim_linked(server, restarting) {
                Ok(claim) => claim,
                Err(refusal) => {
                    lifecycle.settle_pair_start(operation, true);
                    ui.invoke_present_toast(refusal.message().into(), true);
                    return;
                }
            };
            stamp_host_lifecycle(&ui, &lifecycle);
            (operation, claim)
        };
        connect.forget_last_run(InstalledHost::Instance(client));
        send(
            &connect.commands,
            Command::LaunchAndConnect {
                client,
                server,
                operation,
            },
        );
        if claim == LinkedClaim::Starting {
            connect.reopen_server_console(server);
        }
        connect.show_pair(
            &ui,
            InstalledHost::Instance(client),
            InstalledHost::Server(server),
        );
    });
}

/// Splitting the workspace, closing its panes, and pointing a pane at a host.
fn register_pane_layout(ui: &AppWindow, workspace: &Workspace) {
    let weak = ui.as_weak();
    let beside = workspace.clone();
    ui.on_split_pane_beside(move |pane| {
        let (Some(ui), Some(pane)) = (weak.upgrade(), workspace::pane_id(pane)) else {
            return;
        };
        beside.change_pane(&ui, pane, |layout| layout.split_beside(pane).is_ok());
    });

    let weak = ui.as_weak();
    let below = workspace.clone();
    ui.on_split_pane_below(move |pane| {
        let (Some(ui), Some(pane)) = (weak.upgrade(), workspace::pane_id(pane)) else {
            return;
        };
        below.change_pane(&ui, pane, |layout| layout.split_below(pane).is_ok());
    });

    let weak = ui.as_weak();
    let close = workspace.clone();
    ui.on_close_pane(move |pane| {
        let (Some(ui), Some(pane)) = (weak.upgrade(), workspace::pane_id(pane)) else {
            return;
        };
        close.change_pane(&ui, pane, |layout| layout.close(pane).is_ok());
    });

    let weak = ui.as_weak();
    let choose = workspace.clone();
    ui.on_choose_pane_host(move |pane, kind, id| {
        let (Some(ui), Some(pane), Some(host)) = (
            weak.upgrade(),
            workspace::pane_id(pane),
            workspace::parse_host(&kind, &id),
        ) else {
            return;
        };
        choose.change_pane(&ui, pane, |layout| layout.show_host(pane, host).is_ok());
    });
}

/// How a fresh Launch or Start of a host goes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HostStart {
    Alone,
    /// A pair start with this operation.
    Paired {
        operation: u64,
        client: InstalledHost,
        server: InstalledHost,
        /// Whether this start begins a new run of the server, as opposed to
        /// joining one that is already running.
        starts_server: bool,
    },
    /// The start was refused, and the reason is said to the user.
    Refused(StartRefusal),
}

/// Why a fresh Launch or Start of a paired host was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StartRefusal {
    /// A start of the host's pair is already under way.
    PairStarting,
    /// The pair's server could not be claimed for the start.
    Server(LinkedRefusal),
}

impl StartRefusal {
    fn message(&self) -> String {
        match self {
            Self::PairStarting => lookup("gui-pair-start-busy"),
            Self::Server(refusal) => refusal.message(),
        }
    }
}

/// Launching a client and starting a server, each of which starts its pair
/// when the pair starts together. A pair start puts both sides on screen, and
/// starting a server alone puts its console on screen.
///
/// Start is offered from the server card, the workspace header, and the
/// overview, and every one of them would otherwise have to navigate for itself.
/// Done once here, so all of them land on the output the run is about to
/// produce.
fn register_host_starts(ui: &AppWindow, workspace: &Workspace) {
    let weak = ui.as_weak();
    let launch = workspace.clone();
    ui.on_launch_client(move |id| {
        let (Some(ui), Some(instance)) = (weak.upgrade(), backend::parse_instance_id(&id)) else {
            return;
        };
        let host = InstalledHost::Instance(instance);
        match launch.claim_start(&ui, host) {
            HostStart::Alone => send(
                &launch.commands,
                Command::LaunchClient {
                    instance,
                    account: None,
                    additional: None,
                },
            ),
            HostStart::Paired {
                operation,
                client,
                server,
                starts_server,
            } => {
                send(
                    &launch.commands,
                    Command::StartLinked {
                        initiator: host,
                        operation,
                    },
                );
                if starts_server && let InstalledHost::Server(id) = server {
                    launch.reopen_server_console(id);
                }
                launch.show_pair(&ui, client, server);
            }
            HostStart::Refused(refusal) => {
                ui.invoke_present_toast(refusal.message().into(), true);
            }
        }
    });

    // Launching as a chosen account, or beside a running launch, is a start of
    // the client like any other.
    let launch_as = workspace.clone();
    ui.on_launch_client_as(move |id, account, alongside| {
        let Some(instance) = backend::parse_instance_id(&id) else {
            return;
        };
        launch_as.forget_last_run(InstalledHost::Instance(instance));
        // An empty choice is the default account, which is what plain Launch
        // does. Passing it through as a reference would ask the services to
        // resolve the empty string.
        let account = (!account.is_empty()).then(|| account.to_string());
        // A second client needs a handle of its own, so it can be listed and
        // stopped without touching the launch it is running beside.
        let additional = alongside.then(next_launch_handle);
        send(
            &launch_as.commands,
            Command::LaunchClient {
                instance,
                account,
                additional,
            },
        );
    });

    let weak = ui.as_weak();
    let start = workspace.clone();
    ui.on_server_start(move |id| {
        let (Some(ui), Some(server)) = (weak.upgrade(), backend::parse_server_id(&id)) else {
            return;
        };
        let host = InstalledHost::Server(server);
        let (pair, starts_server) = match start.claim_start(&ui, host) {
            HostStart::Alone => {
                if !claim_server_start(&ui, &start, server) {
                    return;
                }
                send(&start.commands, Command::StartServer(server));
                (None, true)
            }
            HostStart::Paired {
                operation,
                client,
                server,
                starts_server,
            } => {
                send(
                    &start.commands,
                    Command::StartLinked {
                        initiator: host,
                        operation,
                    },
                );
                (Some((client, server)), starts_server)
            }
            HostStart::Refused(refusal) => {
                ui.invoke_present_toast(refusal.message().into(), true);
                return;
            }
        };
        if starts_server {
            start.reopen_server_console(server);
        }
        // A pair start shows both sides beside each other instead, which never
        // moves a pane off the host it shows.
        match pair {
            Some((client, server)) => start.show_pair(&ui, client, server),
            None => start.show_console(&ui, host),
        }
    });
}

/// Claims a standalone server start through the shared action guard.
fn claim_server_start(
    ui: &AppWindow,
    workspace: &Workspace,
    server: ephervex_model::ServerId,
) -> bool {
    let mut lifecycle = workspace.lifecycle.borrow_mut();
    let restarting = !lifecycle.stage_of(InstalledHost::Server(server)).is_empty();
    if !lifecycle
        .servers
        .request(server, ServerAction::Start, restarting)
    {
        return false;
    }
    stamp_host_lifecycle(ui, &lifecycle);
    true
}

/// A host workspace's Settings tab: its JVM settings, and a client's
/// pack-owned-mod-mutation opt-in.
fn register_host_runtime_actions(
    ui: &AppWindow,
    commands: &Sender<Command>,
    workspace: &Workspace,
) {
    let sender = commands.clone();
    let views = workspace.clone();
    ui.on_save_host_runtime(move |kind, id, min, max, args| {
        let Some(host) = workspace::parse_host(&kind, &id) else {
            return;
        };
        clear_runtime_error(&views, host);
        send(
            &sender,
            Command::SaveHostRuntime {
                host,
                min_memory_mib: min.to_string(),
                max_memory_mib: max.to_string(),
                jvm_args: args.to_string(),
            },
        );
    });

    let sender = commands.clone();
    let views = workspace.clone();
    ui.on_clear_host_runtime(move |kind, id| {
        if let Some(host) = workspace::parse_host(&kind, &id) {
            clear_runtime_error(&views, host);
            send(
                &sender,
                Command::SaveHostRuntime {
                    host,
                    min_memory_mib: String::new(),
                    max_memory_mib: String::new(),
                    jvm_args: String::new(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_set_allow_pack_mod_version_changes(move |kind, id, allow| {
        if let Some(host) = workspace::parse_host(&kind, &id) {
            send(
                &sender,
                Command::SetAllowPackModVersionChanges { host, allow },
            );
        }
    });
}

/// Forgets why the host's last runtime save was refused, so a new attempt
/// starts from a form with no verdict on it.
fn clear_runtime_error(workspace: &Workspace, host: InstalledHost) {
    workspace.update(host, |view| {
        view.runtime_error = SharedString::default();
    });
}

fn register_server_actions(ui: &AppWindow, commands: &Sender<Command>, workspace: &Workspace) {
    register_host_runtime_actions(ui, commands, workspace);

    let sender = commands.clone();
    ui.on_repair_server(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::RepairServer(id));
        }
    });

    register_name_rules(ui);
    register_server_pack_rules(ui);
    register_server_property_actions(ui, commands, workspace);

    let sender = commands.clone();
    ui.on_open_server_folder(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::OpenServerFolder(id));
        }
    });

    let sender = commands.clone();
    ui.on_open_server_location(move |id, location| {
        let Some(server) = backend::parse_server_id(&id) else {
            return;
        };
        if location == "capture" {
            send(&sender, Command::OpenServerCapture(server));
        } else if let Some(folder) = ephervex_core::ServerFolder::parse(&location) {
            send(&sender, Command::OpenServerLocation { server, folder });
        }
    });

    let sender = commands.clone();
    ui.on_load_server_workspace(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::LoadServerWorkspace(id));
        }
    });

    register_host_starts(ui, workspace);

    let sender = commands.clone();
    let lifecycle = Rc::clone(&workspace.lifecycle);
    let weak = ui.as_weak();
    ui.on_server_stop(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            let mut lifecycle = lifecycle.borrow_mut();
            if !lifecycle.servers.request(id, ServerAction::Stop, false) {
                return;
            }
            if let Some(ui) = weak.upgrade() {
                stamp_host_lifecycle(&ui, &lifecycle);
            }
            send(&sender, Command::StopServer(id));
        }
    });

    let sender = commands.clone();
    let lifecycle = Rc::clone(&workspace.lifecycle);
    let weak = ui.as_weak();
    ui.on_server_kill(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            let mut lifecycle = lifecycle.borrow_mut();
            if !lifecycle.servers.request(id, ServerAction::Kill, false) {
                return;
            }
            if let Some(ui) = weak.upgrade() {
                stamp_host_lifecycle(&ui, &lifecycle);
            }
            send(&sender, Command::KillServer(id));
        }
    });

    register_server_console(ui, commands, &workspace.lifecycle);

    let sender = commands.clone();
    ui.on_accept_eula(move || send(&sender, Command::AcceptEula));
    let sender = commands.clone();
    ui.on_revoke_eula(move || send(&sender, Command::RevokeEula));
    let sender = commands.clone();
    ui.on_open_eula_reference(move || send(&sender, Command::OpenEulaReference));
}

/// All console panes submit through the same per-server action guard.
fn register_server_console(
    ui: &AppWindow,
    commands: &Sender<Command>,
    lifecycle: &Rc<RefCell<HostLifecycle>>,
) {
    let sender = commands.clone();
    let lifecycle = Rc::clone(lifecycle);
    let weak = ui.as_weak();
    ui.on_server_console(move |id, command| {
        let Some(server) = backend::parse_server_id(&id) else {
            return false;
        };
        if command.trim().is_empty() {
            return false;
        }
        let mut lifecycle = lifecycle.borrow_mut();
        let restarting = !lifecycle.stage_of(InstalledHost::Server(server)).is_empty();
        if !lifecycle
            .servers
            .request(server, ServerAction::Console, restarting)
        {
            return false;
        }
        let queued = sender
            .send(Command::SendConsole {
                server,
                command: command.into(),
            })
            .is_ok();
        if !queued {
            lifecycle.servers.apply(
                server,
                backend::ServerToolEvent::ConsoleFinished {
                    outcome: backend::ConsoleOutcome::Rejected,
                },
            );
        }
        if let Some(ui) = weak.upgrade() {
            stamp_host_lifecycle(&ui, &lifecycle);
        }
        queued
    });
}

/// The name suggested for a server linked to a client, answered here so the
/// markup never composes it and cannot drift from what creation accepts.
fn register_name_rules(ui: &AppWindow) {
    ui.global::<NameRules>()
        .on_linked_server_name(|client| backend::suggested_server_name(&client).into());
}

/// Why the create-server dialog's selection disagrees with its server files,
/// answered by the same check creation refuses with.
fn register_server_pack_rules(ui: &AppWindow) {
    ui.global::<ServerPackRules>().on_conflict(
        |minecraft, loader, loader_version, pack_minecraft, pack_loader, pack_version, mods| {
            server_pack_conflict(
                &ServerPackSelection {
                    minecraft: &minecraft,
                    loader: &loader,
                    loader_version: &loader_version,
                },
                &ServerArchiveEvidence {
                    carries_mods: mods,
                    loader: loader_kind_of(&pack_loader),
                    loader_version: (!pack_version.is_empty()).then(|| pack_version.into()),
                    minecraft_version: (!pack_minecraft.is_empty()).then(|| pack_minecraft.into()),
                },
            )
            .into()
        },
    );
}

/// The create-server dialog's selection as its fields hold it.
struct ServerPackSelection<'a> {
    minecraft: &'a str,
    loader: &'a str,
    loader_version: &'a str,
}

/// The localized disagreement, or empty when there is none. A modded selection
/// still waiting for its build says nothing here: the dialog already asks for
/// one, and comparing an empty build would only repeat that.
fn server_pack_conflict(
    selection: &ServerPackSelection<'_>,
    evidence: &ServerArchiveEvidence,
) -> String {
    let loader = match loader_kind_of(selection.loader) {
        None => ephervex_model::LoaderSelection::vanilla(),
        Some(_) if selection.loader_version.is_empty() => return String::new(),
        Some(kind) => ephervex_model::LoaderSelection::exact(kind, selection.loader_version),
    };
    evidence
        .conflict(selection.minecraft, &loader)
        .map(|conflict| {
            let args = conflict.message_args();
            lookup_args(
                conflict.message_id(),
                args.iter()
                    .map(|(name, value)| (*name, Arg::from(value.as_str()))),
            )
        })
        .unwrap_or_default()
}

fn register_server_property_actions(
    ui: &AppWindow,
    commands: &Sender<Command>,
    workspace: &Workspace,
) {
    let rules = ui.global::<PropertyRules>();
    rules.on_port_error(|port| validate_port(&port).message.into());
    rules.on_address_error(|address, custom| validate_ip(&address, custom).message.into());

    let forms = Rc::clone(&workspace.properties);
    let shown = workspace.clone();
    ui.on_edit_server_property(move |id, key, value| {
        if forms.borrow_mut().edit(&id, &key, &value) {
            publish_property_form(&shown, &id, false);
        }
    });
    let forms = Rc::clone(&workspace.properties);
    let shown = workspace.clone();
    ui.on_choose_server_property(move |id, key, index| {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        if forms.borrow_mut().choose(&id, &key, index) {
            publish_property_form(&shown, &id, false);
        }
    });
    let forms = Rc::clone(&workspace.properties);
    let shown = workspace.clone();
    ui.on_show_advanced_server_properties(move |id, show| {
        if forms.borrow_mut().show_advanced(&id, show) {
            publish_property_form(&shown, &id, true);
        }
    });
    let forms = Rc::clone(&workspace.properties);
    let shown = workspace.clone();
    ui.on_discard_server_properties(move |id| {
        if forms.borrow_mut().discard(&id) {
            publish_property_form(&shown, &id, true);
        }
    });

    let sender = commands.clone();
    let forms = Rc::clone(&workspace.properties);
    let weak = ui.as_weak();
    ui.on_save_server_properties(move |id, online_mode, access, address, port| {
        let Some(server) = backend::parse_server_id(&id) else {
            return;
        };
        // The form shows each field's problem as it is typed; a save with
        // one left is refused here all the same.
        let Ok(network) = parse_network(&access, &address, online_mode, &port) else {
            return;
        };
        let Some(values) = forms.borrow().values(&id) else {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_present_toast(lookup("gui-hint-fix-property-values").into(), true);
            }
            return;
        };
        send(
            &sender,
            Command::SaveServerProperties {
                server,
                network,
                values,
            },
        );
    });
}

/// Shows `server`'s Properties form in every pane showing the server. A
/// `fresh` form starts its rows over, which is how a field the user typed in
/// takes the stored value again.
fn publish_property_form(workspace: &Workspace, server: &str, fresh: bool) {
    let Some(form) = workspace.properties.borrow().view(server) else {
        return;
    };
    let Some(id) = backend::parse_server_id(server) else {
        return;
    };
    let groups = property_groups(form.rows);
    workspace.update(InstalledHost::Server(id), |view| {
        view.property_groups = if fresh {
            property_groups_model(groups)
        } else {
            reuse_property_groups(&view.property_groups, groups)
        };
        view.property_edits = i32::try_from(form.edits).unwrap_or(i32::MAX);
        view.property_edits_invalid = !form.edits_valid;
        view.property_show_advanced = form.show_advanced;
    });
}

/// Rows as the cards that show them, split where a group's heading starts.
fn property_groups(
    rows: Vec<server_property_form::PropertyRow>,
) -> Vec<(SharedString, Vec<ServerPropertyRow>)> {
    let mut groups: Vec<(SharedString, Vec<ServerPropertyRow>)> = Vec::new();
    for row in rows {
        if !row.heading.is_empty() || groups.is_empty() {
            groups.push((row.heading.as_str().into(), Vec::new()));
        }
        if let Some((_, rows)) = groups.last_mut() {
            rows.push(server_property_row(row));
        }
    }
    groups
}

/// Shows the `server.properties` defaults page. A `fresh` page starts its
/// rows over, which is how a field the user typed in takes the stored value
/// again.
fn publish_server_defaults(
    ui: &AppWindow,
    form: &server_property_form::ServerDefaultsForm,
    fresh: bool,
) {
    let view = form.view();
    let groups = property_groups(view.rows);
    ui.set_server_defaults_groups(if fresh {
        property_groups_model(groups)
    } else {
        reuse_property_groups(&ui.get_server_defaults_groups(), groups)
    });
    ui.set_server_defaults_edits(i32::try_from(view.edits).unwrap_or(i32::MAX));
    ui.set_server_defaults_invalid(!view.edits_valid);
    ui.set_server_defaults_show_advanced(view.show_advanced);
}

fn register_server_defaults(
    ui: &AppWindow,
    commands: &Sender<Command>,
    drafts: &Rc<RefCell<SettingsDrafts>>,
) {
    let change = |ui: &AppWindow,
                  drafts: &Rc<RefCell<SettingsDrafts>>,
                  fresh: bool,
                  edit: &dyn Fn(&mut server_property_form::ServerDefaultsForm)| {
        let mut drafts = drafts.borrow_mut();
        edit(&mut drafts.server_defaults);
        publish_server_defaults(ui, &drafts.server_defaults, fresh);
    };
    let (weak, shared) = (ui.as_weak(), Rc::clone(drafts));
    ui.on_edit_server_default(move |key, value| {
        if let Some(ui) = weak.upgrade() {
            change(&ui, &shared, false, &|form| form.edit(&key, &value));
        }
    });
    let (weak, shared) = (ui.as_weak(), Rc::clone(drafts));
    ui.on_choose_server_default(move |key, index| {
        let (Some(ui), Ok(index)) = (weak.upgrade(), usize::try_from(index)) else {
            return;
        };
        change(&ui, &shared, false, &|form| form.choose(&key, index));
    });
    let (weak, shared) = (ui.as_weak(), Rc::clone(drafts));
    ui.on_reset_server_default(move |key| {
        if let Some(ui) = weak.upgrade() {
            change(&ui, &shared, true, &|form| form.reset(&key));
        }
    });
    let (weak, shared) = (ui.as_weak(), Rc::clone(drafts));
    ui.on_show_advanced_server_defaults(move |show| {
        if let Some(ui) = weak.upgrade() {
            change(&ui, &shared, true, &|form| form.show_advanced(show));
        }
    });
    let (weak, shared) = (ui.as_weak(), Rc::clone(drafts));
    ui.on_discard_server_defaults(move || {
        if let Some(ui) = weak.upgrade() {
            change(&ui, &shared, true, &|form| form.discard());
        }
    });
    let (weak, shared, sender) = (ui.as_weak(), Rc::clone(drafts), commands.clone());
    ui.on_save_server_defaults(move || {
        let values = shared.borrow().server_defaults.values();
        match values {
            Some(values) => send(&sender, Command::SaveServerPropertyDefaults(values)),
            None => {
                if let Some(ui) = weak.upgrade() {
                    ui.invoke_present_toast(lookup("gui-hint-fix-property-values").into(), true);
                }
            }
        }
    });
}

fn property_groups_model(
    groups: Vec<(SharedString, Vec<ServerPropertyRow>)>,
) -> ModelRc<ServerPropertyGroup> {
    ModelRc::new(VecModel::from(
        groups
            .into_iter()
            .map(|(heading, rows)| ServerPropertyGroup {
                heading,
                rows: ModelRc::new(VecModel::from(rows)),
            })
            .collect::<Vec<_>>(),
    ))
}

/// Writes `groups` into the cards already shown, row by row, so a field the
/// user is typing in keeps its place. Cards that come or go start over.
fn reuse_property_groups(
    model: &ModelRc<ServerPropertyGroup>,
    groups: Vec<(SharedString, Vec<ServerPropertyRow>)>,
) -> ModelRc<ServerPropertyGroup> {
    let same_cards = model.row_count() == groups.len()
        && groups.iter().enumerate().all(|(index, (heading, _))| {
            model
                .row_data(index)
                .is_some_and(|group| group.heading == *heading)
        });
    if !same_cards {
        return property_groups_model(groups);
    }
    for (index, (_, rows)) in groups.into_iter().enumerate() {
        if let Some(group) = model.row_data(index) {
            let _ = workspace::reuse(&group.rows, rows);
        }
    }
    model.clone()
}

fn server_property_row(row: server_property_form::PropertyRow) -> ServerPropertyRow {
    ServerPropertyRow {
        key: row.key.into(),
        label: row.label.into(),
        detail: row.detail.into(),
        kind: row.kind.into(),
        text: row.text.into(),
        placeholder: row.placeholder.into(),
        checked: row.checked,
        choices: ModelRc::new(VecModel::from(
            row.choices
                .into_iter()
                .map(SharedString::from)
                .collect::<Vec<_>>(),
        )),
        choice_index: row.choice_index,
        note: row.note.into(),
        changed: row.changed,
        awaiting_restart: row.awaiting_restart,
        error: row.error.into(),
        can_reset: row.can_reset,
    }
}

fn register_settings(
    ui: &AppWindow,
    commands: &Sender<Command>,
    drafts: &Rc<RefCell<SettingsDrafts>>,
) {
    register_general_settings(ui, commands);
    register_server_defaults(ui, commands, drafts);

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

    register_provider_identifiers(ui, commands, drafts);
    register_curseforge_credential(ui, commands);

    let sender = commands.clone();
    let held = Rc::clone(drafts);
    let weak = ui.as_weak();
    ui.on_save_storage_settings(move |root| {
        let submitted = root.to_string();
        start_settings_save(
            &weak,
            &held,
            &sender,
            &[SettingsSave::StorageRoot],
            move |drafts| drafts.persistent_root.submit(submitted),
            Command::SaveStorageRoot(root.into()),
        );
    });
    register_artifact_cache(ui, commands, drafts);
    let sender = commands.clone();
    ui.on_choose_storage_root(move || send(&sender, Command::PickStorageRoot));
    let sender = commands.clone();
    let held = Rc::clone(drafts);
    let weak = ui.as_weak();
    ui.on_reset_storage_root(move || {
        let shown = weak
            .upgrade()
            .map(|ui| ui.get_persistent_root().to_string())
            .unwrap_or_default();
        start_settings_save(
            &weak,
            &held,
            &sender,
            &[SettingsSave::StorageRoot],
            |drafts| drafts.persistent_root.submit(shown),
            Command::SaveStorageRoot(String::new()),
        );
    });
    register_watched_folders(ui, commands);

    let sender = commands.clone();
    ui.on_load_java_runtimes(move || send(&sender, Command::LoadJavaRuntimes));

    let sender = commands.clone();
    ui.on_refresh_java(move || send(&sender, Command::RefreshJava));

    let sender = commands.clone();
    ui.on_remove_java_runtime(move |path| {
        send(&sender, Command::RemoveManagedJavaRuntime(path.to_string()));
    });

    let sender = commands.clone();
    let weak = ui.as_weak();
    let held = Rc::clone(drafts);
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
            let (min_text, max_text, path_text) = (
                minimum.to_string(),
                maximum.to_string(),
                preferred_path.to_string(),
            );
            start_settings_save(
                &weak,
                &held,
                &sender,
                &[SettingsSave::JavaSettings],
                move |drafts| {
                    drafts.java_auto_detect.submit(auto_detect);
                    drafts.java_auto_download.submit(auto_download);
                    drafts.java_memory_min.submit(min_text);
                    drafts.java_memory_max.submit(max_text);
                    drafts.java_preferred_path.submit(path_text);
                },
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
    keys: &BindingCapture,
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
    let armed = keys.clone();
    ui.on_minecraft_select_category(move |index| {
        let Some(ui) = handle.upgrade() else { return };
        if let Ok(index) = usize::try_from(index) {
            state.borrow_mut().select_category(index);
        }
        end_minecraft_capture(&ui, &state, &armed);
        render_minecraft(&ui, &state);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let armed = keys.clone();
    ui.on_minecraft_set_query(move |query| {
        let Some(ui) = handle.upgrade() else { return };
        state.borrow_mut().set_query(&query);
        end_minecraft_capture(&ui, &state, &armed);
        render_minecraft(&ui, &state);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let armed = keys.clone();
    ui.on_minecraft_set_show_advanced(move |show| {
        let Some(ui) = handle.upgrade() else { return };
        state.borrow_mut().set_show_advanced(show);
        end_minecraft_capture(&ui, &state, &armed);
        render_minecraft(&ui, &state);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let armed = keys.clone();
    ui.on_minecraft_set_only_overridden(move |only| {
        let Some(ui) = handle.upgrade() else { return };
        state.borrow_mut().set_only_overridden(only);
        end_minecraft_capture(&ui, &state, &armed);
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

    // The custom lines are one text box and one edit. Typing records the text
    // without writing it back to the box, which would move the caret.
    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    ui.on_minecraft_edit_raw_options(move |text| {
        let Some(ui) = handle.upgrade() else { return };
        state.borrow_mut().set_raw_options(&text);
        refresh_minecraft_totals(&ui, &state);
    });

    // -- the draft itself.
    register_minecraft_editors(ui, commands, minecraft, keys);
    register_minecraft_capture(ui, commands, minecraft, keys);

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
        let raw_options = state.borrow().raw_edit();
        refresh_minecraft_totals(&ui, &state);
        send(
            &sender,
            Command::SaveMinecraftPreferences { edits, raw_options },
        );
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    let armed = keys.clone();
    ui.on_minecraft_discard_defaults(move || {
        let Some(ui) = handle.upgrade() else { return };
        state.borrow_mut().discard();
        end_minecraft_capture(&ui, &state, &armed);
        render_minecraft(&ui, &state);
        request_minecraft_preview(&ui, &state, &sender);
    });
}

/// The value editors, each of which changes exactly one preference's draft.
fn register_minecraft_editors(
    ui: &AppWindow,
    commands: &Sender<Command>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    keys: &BindingCapture,
) {
    let picker = WindowSizePicker::default();
    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_set_boolean(move |id, on| {
        edit_minecraft_row(&handle, &state, &sender, &id, |model, id| {
            model.set_boolean(id, on);
        });
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_set_number(move |id, number| {
        edit_minecraft_row(&handle, &state, &sender, &id, |model, id| {
            model.set_number(id, f64::from(number));
        });
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_set_enumeration(move |id, index| {
        edit_minecraft_row(&handle, &state, &sender, &id, |model, id| {
            if let Ok(index) = usize::try_from(index) {
                model.set_enumeration(id, index);
            }
        });
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_set_language(move |id, index| {
        edit_minecraft_row(&handle, &state, &sender, &id, |model, id| {
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
        edit_minecraft_row(&handle, &state, &sender, &id, |model, id| {
            model.set_number_text(id, &text);
        });
    });

    register_minecraft_window_picker(ui, commands, minecraft, &picker);
    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    ui.on_minecraft_commit_text(move |id, text| {
        edit_minecraft_row(&handle, &state, &sender, &id, |model, id| {
            model.set_text(id, &text);
        });
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    let following = picker.clone();
    let armed = keys.clone();
    ui.on_minecraft_forget_preference(move |id| {
        // The rows are rebuilt below, which destroys the row being captured.
        // A reset rebuilds because switches and sliders keep their own
        // position after interaction and would not follow a row update.
        if let Some(ui) = handle.upgrade() {
            end_minecraft_capture(&ui, &state, &armed);
        }
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
/// Beginning and canceling deliberately do not rebuild the rows: the control
/// that just took focus lives inside the row, and rebuilding would destroy it
/// mid-capture. That is why the capturing row is named by a property beside
/// the model rather than by a field inside it.
fn register_minecraft_capture(
    ui: &AppWindow,
    commands: &Sender<Command>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    keys: &BindingCapture,
) {
    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let armed = keys.clone();
    ui.on_minecraft_begin_capture(move |id| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(parsed) = minecraft_preference_id(&id) else {
            return;
        };
        state.borrow_mut().begin_capture(&parsed);
        ui.set_minecraft_capturing_id(id);
        armed.arm();
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let armed = keys.clone();
    ui.on_minecraft_cancel_capture(move || {
        let Some(ui) = handle.upgrade() else { return };
        end_minecraft_capture(&ui, &state, &armed);
    });

    // The press itself, where the windowing layer can be reached: the key
    // rather than the character it prints, and a mouse button wherever in the
    // window it was pressed. Nothing arrives here otherwise, and the two
    // toolkit paths below stand in.
    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    let armed = keys.clone();
    keys.listen(move |input| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(parsed) = minecraft_preference_id(&ui.get_minecraft_capturing_id()) else {
            // Nothing is waiting for this press, so nothing may keep taking
            // presses away from the window.
            armed.disarm();
            return;
        };
        let outcome = match input {
            CapturedInput::Key(key) => state.borrow_mut().capture_physical_key(&parsed, key),
            CapturedInput::Mouse(button) => {
                state.borrow_mut().capture_mouse_button(&parsed, button)
            }
        };
        apply_capture_outcome(&ui, &state, &sender, &armed, outcome);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    let armed = keys.clone();
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
        apply_capture_outcome(&ui, &state, &sender, &armed, outcome);
    });

    let handle = ui.as_weak();
    let state = Rc::clone(minecraft);
    let sender = commands.clone();
    let armed = keys.clone();
    ui.on_minecraft_captured_mouse(move |id, reported| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(parsed) = minecraft_preference_id(&id) else {
            return;
        };
        let outcome = state.borrow_mut().capture_mouse(&parsed, &reported);
        apply_capture_outcome(&ui, &state, &sender, &armed, outcome);
    });
}

/// Closes any open capture in the model, on screen, and in the press reader.
///
/// Every way a capture can end goes through here. The reader takes every key
/// and mouse press away from the window while armed, so ending a capture
/// anywhere else would leave the window deaf with no row to bind.
fn end_minecraft_capture(
    ui: &AppWindow,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    keys: &BindingCapture,
) {
    keys.disarm();
    minecraft.borrow_mut().cancel_capture();
    ui.set_minecraft_capturing_id(SharedString::default());
}

/// What the screen does once a capture has resolved.
///
/// A refused press leaves capture open and changes nothing, so the user can
/// simply press something else. Every other outcome ends capture.
fn apply_capture_outcome(
    ui: &AppWindow,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    commands: &Sender<Command>,
    keys: &BindingCapture,
    outcome: minecraft_defaults::CaptureOutcome,
) {
    use minecraft_defaults::CaptureOutcome;

    if outcome == CaptureOutcome::Refused {
        ui.invoke_present_toast(lookup("gui-minecraft-capture-refused").into(), true);
        return;
    }
    end_minecraft_capture(ui, minecraft, keys);
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
/// the other loses the field it just moved to, and how a slider, switch or
/// dropdown stops taking keys after the first one.
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
/// Shows a host's isolation state in every pane showing the host, answering
/// whether the workspace still shows it.
fn apply_isolation_data(workspace: &Workspace, data: IsolationData) -> bool {
    let Some(host) = backend::parse_mod_host(&data.host_kind, &data.host) else {
        return false;
    };
    if !workspace.state.borrow().shown().contains(&host) {
        return false;
    }
    let is_setup = data.phase == "setup";
    let is_inactive = data.phase == "none";
    let mut flow = workspace.isolation.borrow_mut();
    if flow.setup(host).is_none() {
        flow.begin(host);
    }
    let Some(setup) = flow.setup_mut(host) else {
        return false;
    };
    setup.unreconciled = !data.recovery.is_empty();
    setup.setup_basis = data.basis.filter(|_| is_setup);
    data.phase.clone_into(&mut setup.phase);
    if is_setup {
        setup.candidates.clone_from(&data.candidates);
        // Reduced confidence is an explicit choice for each new session, not
        // a preference that carries over from an earlier setup.
        setup.query.clear();
    } else if is_inactive {
        // No session exists, so a setup on screen was planned from a host
        // that has since changed, such as by a snapshot restore.
        setup.forget_setup();
    }
    let candidates = isolation_candidate_rows(setup);
    drop(flow);
    let workflow = isolation_workflow(&data);
    workspace.update(host, |view| {
        view.isolation_candidates = model(candidates);
        view.isolation_enabled = string_model(data.enabled);
        view.isolation_disabled = string_model(data.disabled);
        view.isolation_unrecorded = string_model(data.unrecorded);
        view.isolation_unidentifiable = string_model(data.unidentifiable);
        view.isolation_missing = string_model(data.missing);
        view.isolation_linked_servers = string_model(data.linked_servers);
        view.isolation_linked_unrecorded = string_model(data.linked_unrecorded);
        view.isolation_linked_missing = string_model(data.linked_missing);
        view.isolation_followed_servers = string_model(data.followed_servers);
        view.isolation_untested_server_mods = string_model(data.untested_server_mods);
        view.isolation_led_by = data.led_by.into();
        view.isolation_remaining_candidates = string_model(data.remaining_candidates);
        view.isolation_pinned = string_model(data.pinned);
        view.isolation_required_dependencies = string_model(data.required_dependencies);
        view.isolation_history = string_model(data.history);
        view.isolation_recovery = data.recovery.into();
        view.isolation_recovery_detail = data.recovery_detail.into();
        view.isolation_phase = data.phase.into();
        view.isolation_summary = data.summary.into();
        view.isolation_confidence = data.confidence.into();
        view.isolation_round = to_i32(data.round);
        view.isolation_granularity = to_i32(data.granularity);
        view.isolation_active = data.active;
        view.isolation_reduced_confidence = data.reduced_confidence;
        if is_setup {
            view.isolation_accept_unrecorded = false;
            view.isolation_accept_missing = false;
            view.isolation_sync_linked_servers = false;
        }
        if is_setup || is_inactive {
            view.isolation_query = SharedString::default();
        }
        view.workflow_id = workflow.id;
        view.workflow_surface = workflow.surface;
        view.workflow_title = workflow.title;
        view.workflow_summary = workflow.summary;
        view.workflow_actions = workflow.actions;
        // Whatever refused an earlier change is answered by the state read now.
        view.isolation_error = SharedString::default();
    });
    true
}

/// Claims the in-flight isolation change and shows it on that host's surfaces
/// before the command is sent, so a second click finds it already taken.
fn claim_isolation_change(
    ui: &slint::Weak<AppWindow>,
    state: &mut IsolationFlowState,
    host: InstalledHost,
    mutation: IsolationMutation,
) -> Option<u64> {
    if state.pending.is_none() && refuse_while_host_changes(ui) {
        return None;
    }
    let request = state.dispatch(host, mutation)?;
    if let Some(ui) = ui.upgrade() {
        ui.set_host_busy_kind(isolation_host_kind(host).into());
        ui.set_host_busy_id(host.id().into());
        ui.set_host_busy_text(lookup(mutation.busy_message()).into());
    }
    Some(request)
}

fn clear_host_busy(ui: &AppWindow) {
    ui.set_host_busy_kind(SharedString::default());
    ui.set_host_busy_id(SharedString::default());
    ui.set_host_busy_text(SharedString::default());
}

/// Shows the pending change's reported stage, such as the safety snapshot
/// being taken, instead of its fixed description.
fn apply_isolation_stage(ui: &AppWindow, state: &IsolationFlowState, activity: &ActivityData) {
    let Some(pending) = state.pending else {
        return;
    };
    let Some(operation) = pending.mutation.operation() else {
        return;
    };
    let running = matches!(activity.item.status_kind, "queued" | "running");
    if running
        && activity.entity == pending.host.id()
        && activity.item.title == lookup(operation)
        && !activity.item.detail.is_empty()
    {
        ui.set_host_busy_text(activity.item.detail.clone().into());
    }
}

/// Refuses a new host change while another one owns the host-change state,
/// saying so, because every surface reads that one state.
fn refuse_while_host_changes(ui: &slint::Weak<AppWindow>) -> bool {
    let Some(ui) = ui.upgrade() else {
        return false;
    };
    if ui.get_host_busy_id().is_empty() {
        return false;
    }
    ui.invoke_present_toast(lookup("gui-host-change-wait").into(), true);
    true
}

const fn isolation_host_kind(host: InstalledHost) -> &'static str {
    match host {
        InstalledHost::Instance(_) => "client",
        InstalledHost::Server(_) => "server",
    }
}

/// Retires a setup planned before the host's mods changed.
///
/// Only a setup is affected: an active session is durable and is never
/// discarded here, and a start already sent carries the plan it was given.
fn apply_isolation_basis(workspace: &Workspace, host: InstalledHost, basis: u64) {
    let flow = workspace.isolation.borrow();
    let stale = flow.pending.is_none()
        && flow.setup(host).is_some_and(|setup| {
            setup.phase == "setup" && setup.setup_basis.is_some_and(|planned| planned != basis)
        });
    drop(flow);
    if stale {
        discard_isolation_setup(workspace, host);
    }
}

/// Returns a host's Isolation surface to its empty state without leaving it.
fn discard_isolation_setup(workspace: &Workspace, host: InstalledHost) {
    workspace.isolation.borrow_mut().discard(host);
    workspace.update(host, |view| {
        view.isolation_candidates = model(Vec::new());
        view.isolation_phase = "none".into();
        view.isolation_query = SharedString::default();
        view.isolation_error = SharedString::default();
    });
}

/// A setup's candidates through its filter, each marked when it is pinned.
fn isolation_candidate_rows(setup: &IsolationSetup) -> Vec<IsolationCandidateRow> {
    let query = setup.query.trim().to_lowercase();
    setup
        .candidates
        .iter()
        .filter(|candidate| {
            query.is_empty()
                || candidate.name.to_lowercase().contains(&query)
                || candidate.path.to_lowercase().contains(&query)
        })
        .map(|candidate| IsolationCandidateRow {
            selected: setup
                .pinned
                .contains(&mod_path_key::decode(&candidate.path)),
            path: candidate.path.clone().into(),
            display_path: candidate.display_path.clone().into(),
            name: candidate.name.clone().into(),
        })
        .collect()
}

fn publish_isolation_candidates(
    workspace: &Workspace,
    host: InstalledHost,
    setup: &IsolationSetup,
) {
    let rows = isolation_candidate_rows(setup);
    let query = SharedString::from(setup.query.as_str());
    workspace.update(host, |view| {
        view.isolation_candidates = model(rows);
        view.isolation_query = query;
    });
}

/// What an isolation session puts in its host's workflow area.
struct WorkflowPresentation {
    id: SharedString,
    /// The surface that shows the workflow in full.
    surface: SharedString,
    title: SharedString,
    summary: SharedString,
    actions: ModelRc<WorkflowActionRow>,
}

fn isolation_workflow(data: &IsolationData) -> WorkflowPresentation {
    let action = |id: &'static str,
                  message: &'static str,
                  tone: &'static str,
                  requires_stopped: bool| WorkflowActionRow {
        id: id.into(),
        label: lookup(message).into(),
        tone: tone.into(),
        enabled: true,
        requires_stopped,
    };
    let mut actions = vec![action(
        "details",
        "gui-isolation-detailed-view",
        "secondary",
        false,
    )];
    match data.phase.as_str() {
        // Until the round's mods are back, only the detailed view is offered.
        _ if !data.recovery.is_empty() => {}
        "round" => {
            actions.push(action(
                "gone",
                "gui-isolation-issue-gone",
                "secondary",
                true,
            ));
            actions.push(action(
                "present",
                "gui-isolation-issue-present",
                "primary",
                true,
            ));
            if !data.history.is_empty() {
                actions.push(action("undo", "gui-isolation-undo", "secondary", true));
            }
        }
        "contradiction" => actions.push(action("undo", "gui-isolation-undo", "primary", true)),
        "ready" => {
            actions.push(action("undo", "gui-isolation-undo", "secondary", true));
            actions.push(action("finish", "gui-isolation-finish", "primary", true));
        }
        "result" => actions.push(action("dismiss", "gui-close", "primary", false)),
        _ => {}
    }
    // A kept result leaves the host on the tested mods, so it stays in view
    // until it is closed.
    let kept = data.phase == "result";
    WorkflowPresentation {
        id: if data.active || kept {
            ISOLATION_WORKFLOW.into()
        } else {
            SharedString::default()
        },
        surface: Surface::Isolation.id().into(),
        title: if data.active {
            lookup("gui-isolation-active").into()
        } else if kept {
            lookup("gui-isolation-finished").into()
        } else {
            SharedString::default()
        },
        summary: if kept {
            lookup("gui-isolation-finished-detail").into()
        } else if data.recovery.is_empty() {
            data.summary.as_str().into()
        } else {
            lookup("gui-isolation-recovery-summary").into()
        },
        actions: model(actions),
    }
}

fn string_model(values: Vec<String>) -> ModelRc<SharedString> {
    model(values.into_iter().map(SharedString::from).collect())
}

/// Renders a fresh view of everything core holds.
///
/// The Minecraft defaults screen asks its two background questions again from
/// here, because a refreshed baseline can change both what the draft is and
/// what a release would do with it.
/// Applies a snapshot, handing back every host's row for the workspace.
fn apply_snapshot_event(
    ui: &AppWindow,
    snapshot: Snapshot,
    creations: &Rc<RefCell<CreationTracker>>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    settings_drafts: &Rc<RefCell<SettingsDrafts>>,
    instance_list: &Rc<RefCell<InstanceListState>>,
    commands: &Sender<Command>,
) -> (Vec<InstanceRow>, Vec<ServerRow>) {
    let rows = apply_snapshot(
        ui,
        snapshot,
        minecraft,
        &mut settings_drafts.borrow_mut(),
        &mut instance_list.borrow_mut(),
    );
    apply_pending_creations(ui, &creations.borrow());
    request_minecraft_preview(ui, minecraft, commands);
    request_minecraft_languages(ui, minecraft, commands, false);
    rows
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
                render_minecraft_in_place(ui, minecraft);
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
                render_minecraft_in_place(ui, minecraft);
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
    ui.set_minecraft_preference_rows(model(minecraft.borrow().rows()));
    render_minecraft_summary(ui, minecraft);
}

/// Shows what a metadata answer changed without recreating any row's control.
///
/// The answers arrive after the edit that asked for them, while the user is
/// still in the control they edited. When the same rows are on screen each is
/// written back in place; a different set of rows is replaced.
fn render_minecraft_in_place(ui: &AppWindow, minecraft: &Rc<RefCell<MinecraftDefaultsModel>>) {
    let rows = minecraft.borrow().rows();
    let shown = ui.get_minecraft_preference_rows();
    let same_rows = shown.row_count() == rows.len()
        && rows.iter().enumerate().all(|(index, row)| {
            shown
                .row_data(index)
                .is_some_and(|shown| shown.id == row.id)
        });
    if same_rows {
        for (index, row) in rows.into_iter().enumerate() {
            shown.set_row_data(index, row);
        }
    } else {
        ui.set_minecraft_preference_rows(model(rows));
    }
    render_minecraft_summary(ui, minecraft);
}

/// Shows everything the draft decides apart from the rows themselves.
fn render_minecraft_summary(ui: &AppWindow, minecraft: &Rc<RefCell<MinecraftDefaultsModel>>) {
    let state = minecraft.borrow();
    ui.set_minecraft_category_index(to_i32(state.category_index()));
    ui.set_minecraft_has_advanced(state.has_advanced_rows());
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
    ui.set_minecraft_raw_options(state.raw_options().into());
    drop(state);
    refresh_minecraft_totals(ui, minecraft);
}

/// Refreshes one row in place, leaving every other row's element alone.
///
/// Replacing the whole list rebuilds the elements in it, which would take the
/// pointer's grip, or the keyboard focus, on a control mid-gesture with it.
/// Only the row that changed is written back. An edit that changes which rows
/// the filter shows leaves no row to write into, so the list is replaced then.
fn refresh_minecraft_row(
    ui: &AppWindow,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    id: &ephervex_core::PreferenceId,
) {
    let state = minecraft.borrow();
    let shown = ui.get_minecraft_preference_rows();
    match state.row_at(id) {
        Some((index, row)) if state.visible_entries().len() == shown.row_count() => {
            shown.set_row_data(index, row);
        }
        _ => ui.set_minecraft_preference_rows(model(state.rows())),
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
    ui.set_minecraft_raw_problems(state.raw_problems().into());
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

/// The General page's single Save, which carries both of its sections.
///
/// Each value is checked before any of them is sent, and the toast names
/// whichever one the view could not produce a usable value for: a page that
/// saved what it could and said nothing about the rest would leave the user
/// looking at a setting that did not take.
fn register_general_settings(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_save_general_settings(
        move |text_scale, technical_text_size, theme, show_globally, make_operator, level| {
            let display = u16::try_from(text_scale)
                .ok()
                .and_then(ephervex_core::ApplicationTextScale::from_percent)
                .zip(
                    u16::try_from(technical_text_size)
                        .ok()
                        .and_then(ephervex_core::TechnicalTextSize::from_pixels),
                )
                .zip(ephervex_core::ApplicationTheme::from_stored(theme.as_str()));
            let operator_level = u8::try_from(level)
                .ok()
                .and_then(ephervex_model::OperatorPermissionLevel::from_value);
            match (display, operator_level) {
                (Some(((text_scale, technical_text_size), theme)), Some(operator_level)) => {
                    send(
                        &sender,
                        Command::SaveGeneralSettings {
                            text_scale,
                            technical_text_size,
                            theme,
                            show_globally,
                            make_operator,
                            operator_level,
                        },
                    );
                }
                (None, _) => {
                    if let Some(ui) = weak.upgrade() {
                        ui.invoke_present_toast(
                            lookup("gui-display-settings-invalid").into(),
                            true,
                        );
                    }
                }
                (_, None) => {
                    if let Some(ui) = weak.upgrade() {
                        ui.invoke_present_toast(lookup("gui-operator-level-invalid").into(), true);
                    }
                }
            }
        },
    );

    let sender = commands.clone();
    ui.on_set_dependency_warning(move |warn| {
        send(&sender, Command::SaveDependencyWarning(warn));
    });

    let sender = commands.clone();
    ui.on_set_dependency_following(move |follow| {
        send(&sender, Command::SaveDependencyFollowing(follow));
    });

    let sender = commands.clone();
    ui.on_set_seasonal_themes(move |enabled| {
        send(&sender, Command::SaveSeasonalThemes(enabled));
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
    workspace: &WorkspaceState,
    commands: &Sender<Command>,
) {
    let Some(host) = backend::parse_mod_host(&plan.host_kind, &plan.host) else {
        return;
    };
    // A list still on screen, not merely the one they dropped onto: a plan
    // that arrives after they have navigated away has nowhere to land.
    if !ui.get_workspace_open() || !workspace.shows(host, Surface::Mods) {
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

    if !make_room_for_dialog(ui) {
        return;
    }
    ui.set_mod_clashes(
        plan.clashing
            .iter()
            .map(|clash| LocalModClashRow {
                source: clash.source.display().to_string().into(),
                file_name: clash.file_name.clone().into(),
                existing_is_disabled: clash.existing_is_disabled,
                existing_is_recorded: clash.existing_is_recorded,
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
    ui.invoke_present_toast(detail.into(), true);
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
    if !make_room_for_dialog(ui) {
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
    if !make_room_for_dialog(ui) {
        return;
    }
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

/// Shows a failed run's notice, unless the host has been started over since
/// the run it is about.
fn apply_run_failure(
    workspace: &Workspace,
    lifecycle: &RefCell<HostLifecycle>,
    host: InstalledHost,
    failure: String,
    remedy: Option<ephervex_core::Remedy>,
    view: Option<Box<crate::diagnosis_view::DiagnosisView>>,
    revision: backend::RunRevision,
) {
    let current = match (host, revision) {
        (InstalledHost::Server(server), backend::RunRevision::Server(revision)) => {
            lifecycle.borrow().servers.failure_revision(server) == Some(revision)
        }
        (_, backend::RunRevision::Client(ended)) => workspace
            .diagnosis
            .borrow()
            .client_run_is_current(host, ended),
        _ => true,
    };
    if current {
        diagnosis_flow::apply_run_failed(
            workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            failure,
            remedy,
            view.map(|view| *view),
        );
    }
}

/// Applies a restart's reported stage to every view of the host.
fn apply_host_restart(
    ui: &AppWindow,
    workspace: &Workspace,
    lifecycle: &Rc<RefCell<HostLifecycle>>,
    host: InstalledHost,
    request: u64,
    stage: Option<backend::RestartStage>,
) {
    // The supervisor reports the move to starting once the old run's output is
    // all delivered, so the console starts over with the restarted run and
    // none of the stopped one.
    if let (InstalledHost::Server(server), Some(backend::RestartStage::Starting)) = (host, stage) {
        workspace.reopen_server_console(server);
    }
    if lifecycle.borrow_mut().apply(host, request, stage) {
        stamp_host_lifecycle(ui, &lifecycle.borrow());
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn apply_event(
    ui: &AppWindow,
    event: Event,
    activities: &Rc<RefCell<Vec<ActivityData>>>,
    creations: &Rc<RefCell<CreationTracker>>,
    import_flow: &Rc<RefCell<ImportFlowState>>,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    settings_drafts: &Rc<RefCell<SettingsDrafts>>,
    local_mods: &Rc<RefCell<LocalModDrop>>,
    lifecycle: &Rc<RefCell<HostLifecycle>>,
    disk_usage: &Rc<RefCell<DiskUsageState>>,
    instance_list: &Rc<RefCell<InstanceListState>>,
    workspace: &Workspace,
    commands: &Sender<Command>,
) {
    match event {
        Event::LocalModPlan(plan) => {
            apply_local_mod_plan(ui, &plan, local_mods, &workspace.state.borrow(), commands);
        }
        Event::AbsentMods(absent) => apply_absent_mods(ui, &absent, commands),
        Event::PromotionPlan(plan) => apply_promotion_plan(ui, &plan),
        event @ (Event::PackUrlConsent { .. }
        | Event::PackUrlDownloading { .. }
        | Event::PackDownloaded { .. }
        | Event::PackUrlEnded { .. }) => apply_pack_link_event(ui, event, import_flow, commands),
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
        Event::ServerTool { server, event } => {
            apply_server_tool_event(ui, workspace, server, event);
        }
        Event::Snapshot(snapshot) => {
            let (clients, servers) = apply_snapshot_event(
                ui,
                *snapshot,
                creations,
                minecraft,
                settings_drafts,
                instance_list,
                commands,
            );
            lifecycle.borrow_mut().servers.observe(&servers);
            workspace.state.borrow_mut().set_rows(clients, servers);
            workspace.refresh(ui);
            // A snapshot rebuilds the rows from records that know nothing of
            // restarts, so their stages are stamped again.
            stamp_host_lifecycle(ui, &lifecycle.borrow());
        }
        Event::SettingsSaveFinished { save, succeeded } => {
            let mut drafts = settings_drafts.borrow_mut();
            finish_settings_save(ui, &mut drafts, save, succeeded);
            show_settings_saving(ui, &drafts);
            drop(drafts);
            // The launcher-wide Java settings decide every host's Java that
            // pins none of its own and whether the launcher preference pins a
            // listed runtime. The list is read again, and with it each shown
            // host.
            if succeeded && save == SettingsSave::JavaSettings {
                send(&workspace.commands, Command::LoadJavaRuntimes);
            }
        }
        Event::MinecraftPreferencesSaveFinished { .. }
        | Event::MinecraftPreferencePreview { .. }
        | Event::MinecraftLanguages { .. } => {
            apply_minecraft_event(ui, event, minecraft, commands);
        }
        Event::ArtifactCacheStatus(status) => apply_artifact_cache_status(ui, status),
        Event::HostRuntime(runtime) => {
            // Kept with the host it belongs to, so a late answer lands in its
            // own view and never in the fields of another host.
            workspace.update(runtime.host, |view| {
                view.runtime_memory_min = runtime.min_memory.into();
                view.runtime_memory_max = runtime.max_memory.into();
                view.runtime_jvm_args = runtime.jvm_args.into();
                view.runtime_pinned = runtime.pinned;
                view.runtime_global = runtime.global.into();
                view.allow_pack_mod_changes = runtime.allow_pack_mod_version_changes;
                view.pack_mod_changes_fixed = runtime.pack_mod_changes_fixed;
                view.runtime_revision = view.runtime_revision.wrapping_add(1);
            });
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
        Event::ImportLinkedServerStarted {
            request_id,
            instance,
            server_name,
        } => {
            creations
                .borrow_mut()
                .hand_off_to_linked_server(&request_id, &instance, &server_name);
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
        Event::LoaderVersionsFailed {
            instance,
            loader,
            minecraft,
            message,
        } => apply_loader_versions_failure(ui, instance, loader, &minecraft, message),
        Event::LoaderChangeFinished { instance } => {
            apply_loader_change_finished(ui, instance);
            // The loader decides which Java the instance expects.
            java_runtime::load(&workspace.commands, InstalledHost::Instance(instance), true);
        }
        Event::LoaderChangeFailed { instance, message } => {
            apply_loader_change_failure(ui, instance, message);
        }
        Event::ManagedMods(data) => {
            // Kept with the host it belongs to, checked by kind as well as id,
            // so a server's list never lands in the panel of the instance it
            // is linked to. A host no pane shows keeps nothing; opening it
            // again asks again.
            if let Some(host) = backend::parse_mod_host(&data.host_kind, &data.host)
                && workspace.mods.borrow_mut().apply_list(
                    host,
                    managed_mod_rows(data.mods),
                    data.notice,
                    data.mods_are_the_users,
                )
            {
                mod_views::publish(workspace, &workspace.mods.borrow(), host);
            }
        }
        Event::CrashNotice(notice) => {
            if let Some(host) = backend::parse_mod_host(&notice.host_kind, &notice.host) {
                workspace.update(host, |view| {
                    view.crash_report = notice.report.into();
                    view.crash_failure = notice.failure.into();
                    view.crash_client_code_on_server = notice.client_code_on_server;
                    view.crash_suggested = i32::try_from(notice.suggested).unwrap_or(i32::MAX);
                });
            }
        }
        Event::Running(running) => {
            ui.set_running(ModelRc::new(VecModel::from(
                running
                    .into_iter()
                    .map(|process| RunningRow {
                        icon_slot: workspace
                            .state
                            .borrow()
                            .icon_slot(&process.kind, &process.id),
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
        Event::ModAcquisitionRequired(data) => apply_mod_acquisition(ui, data, workspace),
        Event::ModAcquisitionEnded { id } => end_mod_acquisition(ui, id, workspace),
        Event::ModStandInOffered(data) => apply_mod_stand_in(ui, data, workspace),
        Event::ModStandInEnded { id } => end_mod_stand_in(ui, id, workspace),
        Event::ModRelations(data) => apply_mod_relations(ui, &data),
        Event::ModRemovalPlan(data) => apply_mod_removal_plan(ui, data, workspace),
        Event::ModResults(results) => apply_mod_results(workspace, results, commands),
        Event::ModPlan(plan) => apply_mod_install_plan(ui, plan, workspace, commands),
        Event::ModInstallFinished {
            server,
            instance,
            provider,
            project,
        } => {
            let finished = BrowserInstall {
                server,
                instance,
                provider,
                project,
            };
            let next = workspace.browser.installs.borrow_mut().finish(&finished);
            advance_browser_install(commands, workspace, next);
        }
        // The question will never be answered, so whatever asked it stops
        // waiting. The reason itself already arrived as an error.
        Event::ModPlanFailed {
            server,
            instance,
            provider,
            requested,
        } => {
            let abandoned = BrowserInstall {
                server,
                instance,
                provider,
                project: requested,
            };
            let next = workspace.browser.installs.borrow_mut().finish(&abandoned);
            advance_browser_install(commands, workspace, next);
        }
        Event::LinkedApply { request, section } => {
            apply_linked_confirmation(ui, request, section, workspace);
        }
        Event::LinkedResult(result) => apply_linked_result(ui, result),
        Event::ServerWorkspace(data) => apply_server_workspace(workspace, data),
        Event::LiveLog(_) => {}
        Event::Diagnostics {
            rows,
            scope,
            scope_id,
            kind,
        } => workspace
            .logs
            .borrow_mut()
            .apply_listing(&rows, &scope, &scope_id, &kind),
        Event::DiagnosticContent(content) => {
            apply_diagnostic_content(ui, &mut workspace.logs.borrow_mut(), &content);
        }
        Event::DiagnosticUnavailable {
            pane,
            scope,
            name,
            message,
            ..
        } => {
            workspace
                .logs
                .borrow_mut()
                .apply_report_unavailable(pane, scope, &name, &message);
            ui.set_status_message(lookup("gui-diagnostic-disappeared").into());
        }
        Event::HostPackContent(data) => {
            if let Some(host) = backend::parse_mod_host(&data.host_kind, &data.host) {
                let rows = model(
                    data.rows
                        .into_iter()
                        .map(|pack| PackContentRow {
                            kind: pack.kind.into(),
                            name: pack.name.into(),
                            path: pack.path.into(),
                            owner: pack.owner.into(),
                            origin: pack.origin.into(),
                            status: pack.status.into(),
                            missing: pack.missing,
                        })
                        .collect(),
                );
                workspace.update(host, |view| view.pack_content = rows);
            }
        }
        Event::HostSnapshots(data) => {
            if let Some(host) = backend::parse_mod_host(&data.host_kind, &data.host) {
                let rows = model(
                    data.rows
                        .into_iter()
                        .map(|snapshot| SnapshotRow {
                            id: snapshot.id.into(),
                            label: snapshot.label.into(),
                            captured: snapshot.captured.into(),
                            size: snapshot.size.into(),
                            mods: snapshot.mods.into(),
                            origin: snapshot.origin.into(),
                            copy_method: snapshot.copy_method.into(),
                            uses_full_space: snapshot.uses_full_space,
                        })
                        .collect(),
                );
                workspace.update(host, |view| {
                    view.snapshots = rows;
                    view.snapshot_total = data.total.into();
                    view.snapshots_have_plain_copies = data.has_plain_copies;
                });
            }
        }
        Event::Isolation {
            generation,
            open,
            data,
        } => {
            if let Some(host) = backend::parse_mod_host(&data.host_kind, &data.host)
                && workspace.isolation.borrow().accepts(host, generation)
                && apply_isolation_data(workspace, data)
                && open
            {
                workspace.reveal_surface(ui, host, Surface::Isolation);
            }
        }
        Event::JavaRuntimes(rows) => {
            ui.set_java_runtimes(model(java_rows(rows)));
            // A runtime that came or went can change what a host resolves to
            // or whether its pin still runs, so each shown host reads it again.
            java_runtime::reload_shown(workspace);
        }
        Event::Activity(activity) => {
            apply_isolation_stage(ui, &workspace.isolation.borrow(), &activity);
            diagnosis_flow::apply_activity(
                ui,
                workspace,
                &mut workspace.diagnosis.borrow_mut(),
                &activity,
            );
            apply_disk_usage_activity(ui, &mut disk_usage.borrow_mut(), &activity);
            upsert_activity(&mut activities.borrow_mut(), activity);
            apply_activities(ui, &activities.borrow());
        }
        Event::IsolationBasis { host, basis } => {
            apply_isolation_basis(workspace, host, basis);
        }
        Event::IsolationSettled { request } => {
            if workspace.isolation.borrow_mut().settle(request) {
                clear_host_busy(ui);
            }
        }
        Event::DiskUsage { request, report } => {
            apply_disk_usage(ui, &mut disk_usage.borrow_mut(), request, &report);
        }
        Event::DiskUsageSettled { request } => {
            settle_disk_usage(ui, &mut disk_usage.borrow_mut(), request);
        }
        Event::StorageMaintenance(event) => {
            storage_maintenance::apply(ui, &mut disk_usage.borrow_mut(), *event, commands);
        }
        Event::HostRestart {
            host,
            request,
            stage,
        } => apply_host_restart(ui, workspace, lifecycle, host, request, stage),
        // Sent only once the old run is gone, so a refused or unfinished stop
        // keeps the notice.
        Event::ClientRestartStarting {
            instance,
            run_floor,
        } => {
            workspace.forget_last_run_from(InstalledHost::Instance(instance), run_floor);
        }
        Event::PairStartSettled {
            operation,
            server_completion_owned,
        } => apply_pair_start_settled(ui, workspace, lifecycle, operation, server_completion_owned),
        Event::LaunchContext {
            instance,
            restartable,
        } => {
            let key = instance.to_string();
            if restartable {
                lifecycle.borrow_mut().restartable.insert(key);
            } else {
                lifecycle.borrow_mut().restartable.remove(&key);
            }
            stamp_host_lifecycle(ui, &lifecycle.borrow());
        }
        Event::HostDiagnosed {
            host,
            request,
            view,
        } => diagnosis_flow::apply_diagnosed(
            workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            request,
            *view,
        ),
        Event::DiagnosisFailed {
            host,
            request,
            message,
            canceled,
        } => diagnosis_flow::apply_diagnosis_failed(
            workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            request,
            message,
            canceled,
        ),
        Event::FixPreviewed {
            host,
            request,
            preview,
        } => diagnosis_flow::apply_fix_previewed(
            workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            request,
            preview,
        ),
        Event::FixApplied {
            host,
            request,
            outcome,
        } => {
            diagnosis_flow::apply_fix_applied(
                workspace,
                &mut workspace.diagnosis.borrow_mut(),
                host,
                request,
                outcome,
            );
            // Any fix can change what the host's Java resolves to, so the
            // runtime section reads it again rather than showing what was.
            java_runtime::load(&workspace.commands, host, true);
        }
        Event::HostJava(data) => java_runtime::apply_host_java(workspace, &data),
        Event::HostJavaPathChecked { host, result } => {
            java_runtime::apply_path_checked(workspace, host, &result);
        }
        Event::HostJavaInstalled { host, pin } => {
            java_runtime::apply_installed(workspace, host, pin);
        }
        Event::HostJavaInstallSettled(host) => java_runtime::apply_install_settled(workspace, host),
        Event::DiagnosisSettled { request } => {
            diagnosis_flow::settle(
                ui,
                workspace,
                &mut workspace.diagnosis.borrow_mut(),
                request,
            );
        }
        Event::RunFailed {
            host,
            failure,
            remedy,
            view,
            revision,
        } => apply_run_failure(workspace, lifecycle, host, failure, remedy, view, revision),
        Event::BundlePlanned { host, parts } => {
            diagnosis_flow::apply_bundle_plan(
                workspace,
                &mut workspace.diagnosis.borrow_mut(),
                host,
                parts,
            );
        }
        Event::BundleExported {
            host,
            request,
            report,
        } => diagnosis_flow::apply_bundle_exported(
            workspace,
            &workspace.diagnosis.borrow(),
            host,
            request,
            &report,
        ),
        Event::BundleExportFailed {
            host,
            request,
            message,
        } => diagnosis_flow::apply_bundle_export_failed(
            workspace,
            &workspace.diagnosis.borrow(),
            host,
            request,
            &message,
        ),
        Event::LauncherBundlePlanned { parts } => {
            diagnosis_flow::apply_launcher_bundle_plan(
                ui,
                &mut workspace.diagnosis.borrow_mut(),
                parts,
            );
        }
        Event::LauncherBundleExported { request, report } => {
            diagnosis_flow::apply_launcher_bundle_exported(
                ui,
                &workspace.diagnosis.borrow(),
                request,
                &report,
            );
        }
        Event::LauncherBundleExportFailed { request, message } => {
            diagnosis_flow::apply_launcher_bundle_export_failed(
                ui,
                &workspace.diagnosis.borrow(),
                request,
                &message,
            );
        }
        Event::BundleSettled { request } => {
            let mut diagnosis = workspace.diagnosis.borrow_mut();
            diagnosis_flow::settle_bundle(workspace, &mut diagnosis, request);
            diagnosis_flow::settle_launcher_bundle(ui, &mut diagnosis, request);
        }
        Event::Toast { message, error } => {
            ui.invoke_present_toast(message.into(), error);
        }
        Event::ModImpact(impact) => apply_mod_impact(ui, &impact),
        Event::ModRequirements(needs) => apply_mod_requirements(ui, &needs),
        Event::ModalError { scope, message } => apply_modal_error(ui, scope, message),
        Event::HostError {
            host,
            surface,
            message,
        } => apply_host_error(workspace, host, surface, message),
        Event::BackendUnavailable(message) => apply_backend_unavailable(ui, message),
        Event::CloseModal(scope) => close_modal_for(ui, scope),
        // A new instance is what the user was just looking at the dialog for,
        // so it is what they are looking at now. The snapshot that follows this
        // keeps the selection, because it keeps whatever was selected.
        Event::ClientCreated { instance } => {
            reveal_instance(ui, &instance);
        }
        Event::CreationFinished { request_id } => {
            creations.borrow_mut().finish(&request_id);
            apply_pending_creations(ui, &creations.borrow());
            // Installing a server can install the managed runtime it needs.
            java_runtime::reload_shown(workspace);
        }
        Event::JavaPlanned => java_runtime::reload_shown(workspace),
        // A discarded result belongs to an attempt this flow no longer tracks
        // (superseded, the modal was closed and reopened, or an unrelated
        // submission finished): there is nothing that should touch whatever
        // modal is open now. See `apply_import_result`.
        Event::ImportArchivePicked {
            generation,
            archive,
        } => {
            if import_flow.borrow().tracks(generation) {
                send(
                    commands,
                    Command::AnalyzeImport {
                        generation,
                        archive,
                    },
                );
            }
        }
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
            creations.borrow_mut().finish(&request_id);
            apply_pending_creations(ui, &creations.borrow());
            apply_import_submission_finished(ui, import_flow, generation, succeeded);
            // An import can install a linked server, and with it a runtime.
            java_runtime::reload_shown(workspace);
        }
        Event::StorageRootPicked(path) => {
            ui.set_persistent_root(path.display().to_string().into());
        }
        Event::HostIconPicked { host, path } => {
            send(commands, Command::SetHostIcon { host, path });
        }
        Event::ServerPackPicked { target, path } => {
            choose_server_pack(ui, commands, target, Some(&path));
        }
        Event::ServerPackInspected { path, evidence } => {
            apply_server_pack_evidence(ui, &path, evidence);
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
    ui.set_instance_favorites_only(false);
    ui.set_instance_minecraft_index(0);
    ui.set_instance_loader_index(0);
    ui.set_instance_run_index(0);
    ui.set_instance_linking_index(0);
    ui.set_instance_group_index(0);
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
        backend::suggested_server_name(&target)
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

/// Whether the Change Loader dialog is open for `instance`. A later outcome of
/// a change started from an earlier dialog must not act on this one.
fn loader_change_dialog_is_for(ui: &AppWindow, instance: ephervex_model::InstanceId) -> bool {
    ui.get_modal() == "loader-change" && ui.get_mod_action_host() == instance.to_string().as_str()
}

/// Whether the open Change Loader dialog lists builds of `loader` for
/// `minecraft`.
fn loader_change_dialog_lists(
    ui: &AppWindow,
    loader: ephervex_model::LoaderKind,
    minecraft: &str,
) -> bool {
    ui.get_modal() == "loader-change"
        && ui.get_loader_change_loader() == loader.to_string().as_str()
        && ui.get_loader_change_minecraft() == minecraft
}

/// Closes the Change Loader dialog when it is the one the finished change was
/// started from.
fn apply_loader_change_finished(ui: &AppWindow, instance: ephervex_model::InstanceId) {
    if loader_change_dialog_is_for(ui, instance) {
        close_modal_for(ui, backend::ModalScope::LOADER_CHANGE);
    }
}

/// Shows a refused or failed loader change in the dialog it was started from,
/// or as a toast once that dialog is gone or open for another instance.
fn apply_loader_change_failure(
    ui: &AppWindow,
    instance: ephervex_model::InstanceId,
    message: String,
) {
    if loader_change_dialog_is_for(ui, instance) {
        apply_modal_error(ui, backend::ModalScope::LOADER_CHANGE, message);
    } else {
        ui.invoke_present_toast(message.into(), true);
    }
}

/// Ends the Change Loader dialog's wait for its build list with the reason, or
/// tells the user in a toast once that dialog is gone or open for something else.
fn apply_loader_versions_failure(
    ui: &AppWindow,
    instance: ephervex_model::InstanceId,
    loader: ephervex_model::LoaderKind,
    minecraft: &str,
    message: String,
) {
    if loader_change_dialog_is_for(ui, instance)
        && loader_change_dialog_lists(ui, loader, minecraft)
    {
        ui.set_loader_change_loading(false);
        ui.set_modal_error(message.into());
    } else {
        ui.invoke_present_toast(message.into(), true);
    }
}

fn apply_loader_versions(
    ui: &AppWindow,
    form: &str,
    loader: ephervex_model::LoaderKind,
    minecraft: &str,
    versions: &[String],
) {
    let label = loader.to_string();
    // Applied only to the dialog that is open for this loader and release: a
    // list asked for by a dialog since closed must not fill the next one.
    if form == "loader-change" {
        if !loader_change_dialog_lists(ui, loader, minecraft) {
            return;
        }
        ui.set_loader_change_versions(model(
            versions.iter().cloned().map(SharedString::from).collect(),
        ));
        ui.set_loader_change_loading(false);
        // The build the instance runs is the one offered first, when the list
        // has it; otherwise nothing is chosen until the user picks one.
        let current = ui.get_loader_change_current_build();
        if ui.get_loader_change_selected().is_empty()
            && versions.iter().any(|v| current == v.as_str())
        {
            ui.set_loader_change_selected(current);
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
        // A build the chosen server files name stays chosen: the catalog only
        // lists what else exists, and replacing it would contradict the files.
        let current = ui.get_create_server_loader_version();
        let named = !current.is_empty() && current == ui.get_create_server_pack_loader_version();
        ui.set_create_server_loader_versions(rows);
        if !named {
            ui.set_create_server_loader_version(
                versions.first().cloned().unwrap_or_default().into(),
            );
        }
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
    let linked: Vec<PendingEntityRow> = rows(true, false)
        .into_iter()
        .filter(|row| !row.linked_client_id.is_empty())
        .collect();
    // Unfiltered by selection: the overview tab shows one of these next to
    // every instance card whose id matches, not only the one currently open.
    let linked = model(linked);
    ui.set_linked_pending_servers(linked.clone());
    // Each client's workspace renders its own, narrowed here rather than by
    // hiding rows in the markup, which would leave the room they take.
    for view in ui.get_host_views().iter() {
        if view.kind == "client" {
            workspace::reuse(
                &view.pending_servers,
                workspace::pending_for(&linked, &view.id),
            );
        }
    }
    attach_creating_servers(ui);
}

/// Puts what each instance's linked server is doing onto that instance's own
/// row.
///
/// The overview shows it there rather than in a card of its own: the work
/// belongs to one instance, and a card above that instance is a second row
/// for a single thing. Every row is written, including the ones with nothing
/// to say, so a finished creation clears the row it was on.
fn attach_creating_servers(ui: &AppWindow) {
    let creating = ui.get_linked_pending_servers();
    let instances = ui.get_instances();
    for index in 0..instances.row_count() {
        let Some(mut row) = instances.row_data(index) else {
            continue;
        };
        let pending = creating
            .iter()
            .find(|entry| entry.linked_client_id == row.id);
        let (detail, progress, indeterminate) = pending.map_or_else(
            || (SharedString::default(), 0.0, false),
            |entry| {
                // Something, from the moment the work starts: the tracker has
                // no detail to report until the first stage says one, and a
                // row that said nothing would look like no server was coming.
                let detail = if entry.detail.is_empty() {
                    lookup("gui-creating-linked-server").into()
                } else {
                    entry.detail.clone()
                };
                (detail, entry.progress, entry.indeterminate)
            },
        );
        if row.creating_server_detail == detail
            && (row.creating_server_progress - progress).abs() < f32::EPSILON
            && row.creating_server_indeterminate == indeterminate
        {
            continue;
        }
        row.creating_server_detail = detail;
        row.creating_server_progress = progress;
        row.creating_server_indeterminate = indeterminate;
        instances.set_row_data(index, row);
    }
}

/// The non-secret provider identifiers: the Microsoft Client ID and the
/// advanced User-Agent override, both of which round-trip through the view.
fn register_provider_identifiers(
    ui: &AppWindow,
    commands: &Sender<Command>,
    drafts: &Rc<RefCell<SettingsDrafts>>,
) {
    let sender = commands.clone();
    let held = Rc::clone(drafts);
    let weak = ui.as_weak();
    ui.on_save_api_settings(move |client_id, user_agent| {
        let (id_text, agent_text) = (client_id.to_string(), user_agent.to_string());
        start_settings_save(
            &weak,
            &held,
            &sender,
            &[SettingsSave::MicrosoftClientId, SettingsSave::UserAgent],
            move |drafts| {
                drafts.microsoft_client_id.submit(id_text);
                drafts.user_agent.submit(agent_text);
            },
            Command::SaveApiSettings {
                client_id: client_id.into(),
                user_agent: user_agent.into(),
            },
        );
    });

    let sender = commands.clone();
    let held = Rc::clone(drafts);
    let weak = ui.as_weak();
    ui.on_reset_microsoft_client_id(move || {
        let shown = weak
            .upgrade()
            .map(|ui| ui.get_microsoft_client_id().to_string())
            .unwrap_or_default();
        start_settings_save(
            &weak,
            &held,
            &sender,
            &[SettingsSave::MicrosoftClientId],
            |drafts| drafts.microsoft_client_id.submit(shown),
            Command::ResetMicrosoftClientId,
        );
    });
    let sender = commands.clone();
    let held = Rc::clone(drafts);
    let weak = ui.as_weak();
    ui.on_reset_user_agent(move || {
        let shown = weak
            .upgrade()
            .map(|ui| ui.get_user_agent().to_string())
            .unwrap_or_default();
        start_settings_save(
            &weak,
            &held,
            &sender,
            &[SettingsSave::UserAgent],
            |drafts| drafts.user_agent.submit(shown),
            Command::ResetUserAgent,
        );
    });

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
fn register_artifact_cache(
    ui: &AppWindow,
    commands: &Sender<Command>,
    drafts: &Rc<RefCell<SettingsDrafts>>,
) {
    let sender = commands.clone();
    ui.on_measure_artifact_cache(move || send(&sender, Command::MeasureArtifactCache));
    let sender = commands.clone();
    let held = Rc::clone(drafts);
    let weak = ui.as_weak();
    ui.on_save_artifact_cache_limit(move |selection, custom_value, custom_unit| {
        let (value_text, unit_text) = (custom_value.to_string(), custom_unit.to_string());
        start_settings_save(
            &weak,
            &held,
            &sender,
            &[SettingsSave::ArtifactCacheLimit],
            move |drafts| {
                drafts.artifact_cache_limit_index.submit(selection);
                drafts.artifact_cache_custom_value.submit(value_text);
                drafts.artifact_cache_custom_unit.submit(unit_text);
            },
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
}

/// The budget selector, from the configuration file alone.
///
/// Deliberately separate from [`apply_artifact_cache_status`]: the limit is cheap
/// and arrives with every snapshot, while the usage figure costs a walk of the
/// cache and arrives only when something asked for one.
fn apply_artifact_cache_settings(
    ui: &AppWindow,
    settings: &SettingsData,
    drafts: &mut SettingsDrafts,
) {
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
    fold_draft(
        &mut drafts.artifact_cache_limit_index,
        &ui.get_artifact_cache_limit_index(),
        index_for_view(settings.artifact_cache_limit_index),
        |index| ui.set_artifact_cache_limit_index(index),
    );
    fold_draft(
        &mut drafts.artifact_cache_custom_value,
        &ui.get_artifact_cache_custom_value().to_string(),
        settings.artifact_cache_custom_value.clone(),
        |value| ui.set_artifact_cache_custom_value(value.into()),
    );
    fold_draft(
        &mut drafts.artifact_cache_custom_unit,
        &ui.get_artifact_cache_custom_unit().to_string(),
        settings.artifact_cache_custom_unit.clone(),
        |unit| ui.set_artifact_cache_custom_unit(unit.into()),
    );
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
}

/// The theme list and the chosen theme, and the palette and branding of the
/// theme shown, which is a season's while one stands in for the chosen one.
fn apply_theme_settings(
    ui: &AppWindow,
    theme: &ephervex_core::ApplicationTheme,
    seasonal_themes: bool,
    shown: &ephervex_core::seasonal_theme::ShownTheme,
    user_themes: &crate::theme_files::UserThemes,
    branding: &crate::theme_files::BrandingImages,
    themes_folder: &str,
) {
    let choices = crate::palette::theme_choices(&user_themes.themes);
    ui.set_theme_labels(model(
        choices
            .iter()
            .map(|choice| SharedString::from(choice.label.as_str()))
            .collect(),
    ));
    ui.set_theme_ids(model(
        choices
            .iter()
            .map(|choice| SharedString::from(choice.theme.to_stored()))
            .collect(),
    ));
    let chosen = crate::palette::choice_index(&choices, theme);
    ui.set_theme_index(i32::try_from(chosen).unwrap_or_default());
    ui.set_application_theme(theme.to_stored().into());
    ui.set_seasonal_themes(seasonal_themes);
    ui.set_shown_theme(shown.name().into());
    let note = match shown {
        ephervex_core::seasonal_theme::ShownTheme::Seasonal(season) => lookup_args(
            "gui-seasonal-theme-shown",
            [
                ("season", Arg::from(crate::palette::seasonal_label(*season))),
                (
                    "value",
                    Arg::from(
                        choices
                            .get(chosen)
                            .map(|choice| choice.label.clone())
                            .unwrap_or_default(),
                    ),
                ),
            ],
        ),
        ephervex_core::seasonal_theme::ShownTheme::Recorded(_) => String::new(),
    };
    ui.set_seasonal_theme_note(note.into());
    ui.set_themes_folder(themes_folder.into());
    ui.set_theme_problems(model(
        user_themes
            .problems
            .iter()
            .map(|problem| SharedString::from(problem.as_str()))
            .collect(),
    ));
    crate::palette::apply_theme(ui, shown, &user_themes.themes);
    crate::branding::apply_branding(ui, branding);
}

/// Shows `incoming` unless the field holds an edit the user has not saved.
fn fold_draft<T: PartialEq + Clone>(
    field: &mut DraftField<T>,
    shown: &T,
    incoming: T,
    show: impl FnOnce(T),
) {
    if let Some(value) = field.fold(shown, incoming) {
        show(value);
    }
}

/// Puts the stored value back in each field a finished save covers that still shows what was submitted.
fn finish_settings_save(
    ui: &AppWindow,
    drafts: &mut SettingsDrafts,
    save: SettingsSave,
    succeeded: bool,
) {
    match save {
        SettingsSave::JavaSettings => {
            finish_draft(
                &mut drafts.java_auto_detect,
                &ui.get_java_auto_detect(),
                succeeded,
                |value| ui.set_java_auto_detect(value),
            );
            finish_draft(
                &mut drafts.java_auto_download,
                &ui.get_java_auto_download(),
                succeeded,
                |value| ui.set_java_auto_download(value),
            );
            finish_draft(
                &mut drafts.java_memory_min,
                &ui.get_java_memory_min().to_string(),
                succeeded,
                |value| ui.set_java_memory_min(value.into()),
            );
            finish_draft(
                &mut drafts.java_memory_max,
                &ui.get_java_memory_max().to_string(),
                succeeded,
                |value| ui.set_java_memory_max(value.into()),
            );
            finish_draft(
                &mut drafts.java_preferred_path,
                &ui.get_java_preferred_path().to_string(),
                succeeded,
                |value| ui.set_java_preferred_path(value.into()),
            );
        }
        SettingsSave::StorageRoot => finish_draft(
            &mut drafts.persistent_root,
            &ui.get_persistent_root().to_string(),
            succeeded,
            |value| ui.set_persistent_root(value.into()),
        ),
        SettingsSave::MicrosoftClientId => finish_draft(
            &mut drafts.microsoft_client_id,
            &ui.get_microsoft_client_id().to_string(),
            succeeded,
            |value| ui.set_microsoft_client_id(value.into()),
        ),
        SettingsSave::UserAgent => finish_draft(
            &mut drafts.user_agent,
            &ui.get_user_agent().to_string(),
            succeeded,
            |value| ui.set_user_agent(value.into()),
        ),
        SettingsSave::ArtifactCacheLimit => {
            finish_draft(
                &mut drafts.artifact_cache_limit_index,
                &ui.get_artifact_cache_limit_index(),
                succeeded,
                |value| ui.set_artifact_cache_limit_index(value),
            );
            finish_draft(
                &mut drafts.artifact_cache_custom_value,
                &ui.get_artifact_cache_custom_value().to_string(),
                succeeded,
                |value| ui.set_artifact_cache_custom_value(value.into()),
            );
            finish_draft(
                &mut drafts.artifact_cache_custom_unit,
                &ui.get_artifact_cache_custom_unit().to_string(),
                succeeded,
                |value| ui.set_artifact_cache_custom_unit(value.into()),
            );
        }
    }
}

fn finish_draft<T: PartialEq + Clone>(
    field: &mut DraftField<T>,
    shown: &T,
    succeeded: bool,
    show: impl FnOnce(T),
) {
    if let Some(value) = field.finish(shown, succeeded) {
        show(value);
    }
}

/// The fields that hold what the user is typing until they save.
///
/// A snapshot arrives for reasons unrelated to these fields, so each shows the
/// stored value only while it holds no unsaved edit.
fn apply_settings_fields(ui: &AppWindow, settings: &SettingsData, drafts: &mut SettingsDrafts) {
    apply_artifact_cache_settings(ui, settings, drafts);
    let reloaded = drafts
        .server_defaults
        .load(settings.server_property_defaults.clone());
    publish_server_defaults(ui, &drafts.server_defaults, reloaded);
    fold_draft(
        &mut drafts.persistent_root,
        &ui.get_persistent_root().to_string(),
        settings.persistent_root.clone(),
        |root| ui.set_persistent_root(root.into()),
    );
    fold_draft(
        &mut drafts.microsoft_client_id,
        &ui.get_microsoft_client_id().to_string(),
        settings.microsoft_client_id.clone(),
        |id| ui.set_microsoft_client_id(id.into()),
    );
    fold_draft(
        &mut drafts.user_agent,
        &ui.get_user_agent().to_string(),
        settings.user_agent.clone(),
        |agent| ui.set_user_agent(agent.into()),
    );
    fold_draft(
        &mut drafts.java_auto_detect,
        &ui.get_java_auto_detect(),
        settings.java_auto_detect,
        |value| ui.set_java_auto_detect(value),
    );
    fold_draft(
        &mut drafts.java_auto_download,
        &ui.get_java_auto_download(),
        settings.java_auto_download,
        |value| ui.set_java_auto_download(value),
    );
    fold_draft(
        &mut drafts.java_memory_min,
        &ui.get_java_memory_min().to_string(),
        settings.java_memory_min.to_string(),
        |value| ui.set_java_memory_min(value.into()),
    );
    fold_draft(
        &mut drafts.java_memory_max,
        &ui.get_java_memory_max().to_string(),
        settings.java_memory_max.to_string(),
        |value| ui.set_java_memory_max(value.into()),
    );
    fold_draft(
        &mut drafts.java_preferred_path,
        &ui.get_java_preferred_path().to_string(),
        settings.java_preferred_path.clone(),
        |path| ui.set_java_preferred_path(path.into()),
    );
}

fn apply_snapshot(
    ui: &AppWindow,
    snapshot: Snapshot,
    minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    drafts: &mut SettingsDrafts,
    list: &mut InstanceListState,
) -> (Vec<InstanceRow>, Vec<ServerRow>) {
    let Snapshot {
        instances,
        servers,
        groups,
        organization_set_aside,
        instance_sort,
        version_order,
        accounts,
        minecraft_preferences,
        settings,
        eula,
    } = snapshot;
    ui.set_show_linked_servers(settings.show_linked_servers);
    ui.set_follow_mod_dependencies(settings.follow_mod_dependencies);
    ui.set_warn_on_dependency_impact(settings.warn_on_dependency_impact);
    apply_favorite_versions(ui, settings.favorite_minecraft_versions.clone());
    ui.set_make_default_account_operator(settings.make_default_account_operator);
    ui.set_operator_permission_level(i32::from(settings.operator_permission_level));
    if let Some(notice) = organization_set_aside {
        ui.invoke_present_toast(
            lookup_args(
                if notice.restored {
                    "gui-organization-restored"
                } else {
                    "gui-organization-set-aside"
                },
                [("value", Arg::from(notice.set_aside.display().to_string()))],
            )
            .into(),
            true,
        );
    }
    let clients =
        apply_instance_snapshot(ui, instances, groups, instance_sort, version_order, list);
    let servers = apply_server_snapshot(ui, servers);
    ui.set_account_launch_entries(model(account_launch_entries(&accounts)));
    ui.set_accounts(model(account_rows(accounts)));
    // A refresh happens for reasons unrelated to this screen, so the model
    // folds the confirmed state under whatever is unsaved rather than over it.
    minecraft.borrow_mut().apply_snapshot(minecraft_preferences);
    render_minecraft(ui, minecraft);
    apply_settings_fields(ui, &settings, drafts);
    ui.set_storage_root(settings.storage_root.into());
    ui.set_default_persistent_root(settings.default_persistent_root.into());
    ui.set_persistent_root_is_custom(settings.persistent_root_origin.is_override());
    ui.set_watched_download_folders(model(
        settings
            .watched_download_folders
            .into_iter()
            .map(SharedString::from)
            .collect(),
    ));
    ui.set_effective_microsoft_client_id(settings.effective_microsoft_client_id.into());
    ui.set_microsoft_client_id_is_custom(settings.microsoft_client_id_origin.is_override());
    ui.set_effective_user_agent(settings.effective_user_agent.into());
    ui.set_user_agent_is_custom(settings.user_agent_origin.is_override());
    // Presence only. There is deliberately no `set_curseforge_api_key` here:
    // a snapshot must not be able to put a stored secret into a text field.
    ui.set_curseforge_key_configured(settings.curseforge_key.is_configured());
    ui.set_curseforge_key_default_in_use(
        settings.curseforge_key == ephervex_core::CurseForgeKeyState::BuildDefault,
    );
    ui.set_curseforge_build_key_available(settings.curseforge_default_key_available);
    ui.set_default_mod_catalog(settings.default_mod_catalog.clone().into());
    ui.set_configured_mod_catalog(settings.configured_mod_catalog.clone().into());
    ui.set_default_mod_catalog_is_custom(settings.mod_catalog_origin.is_override());
    ui.set_application_text_scale(i32::from(settings.text_scale_percent));
    ui.set_technical_text_size(i32::from(settings.technical_text_size_pixels));
    ui.global::<Theme>()
        .set_text_scale(f32::from(settings.text_scale_percent) / 100.0);
    apply_theme_settings(
        ui,
        &settings.theme,
        settings.seasonal_themes,
        &settings.shown_theme,
        &settings.user_themes,
        &settings.branding,
        &settings.themes_folder,
    );
    ui.set_eula_accepted(eula.accepted);
    ui.set_eula_status(eula.status.into());
    ui.set_loading(false);
    ui.set_status_message(lookup("gui-ready").into());
    (clients, servers)
}

/// Fills the instance list, and returns every instance's row, filtered or not.
fn apply_instance_snapshot(
    ui: &AppWindow,
    instances: Vec<crate::presentation::InstancePresentation>,
    groups: Vec<ephervex_core::InstanceGroup>,
    sort: ephervex_core::InstanceSort,
    order: ephervex_core::VersionOrder,
    list: &mut InstanceListState,
) -> Vec<InstanceRow> {
    let listed = instances
        .into_iter()
        .map(|item| {
            let group = item.facts.group.as_ref();
            let group_index = group
                .and_then(|group| groups.iter().position(|candidate| candidate.id == group.id))
                .map_or(-1, to_i32);
            let row = InstanceRow {
                snapshot_deletion_note: item.snapshot_deletion_note.into(),
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
                stoppable_linked_servers: to_i32(item.stoppable_linked_servers),
                unstoppable_linked_servers: to_i32(item.unstoppable_linked_servers),
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
                connect_reason: item
                    .launch_and_connect
                    .first()
                    .map_or_else(SharedString::default, |connect| {
                        connect.reason.clone().into()
                    }),
                // Filled in from the creation tracker, which a snapshot knows
                // nothing about.
                creating_server_detail: SharedString::default(),
                creating_server_progress: 0.0,
                creating_server_indeterminate: false,
                restart_stage: SharedString::default(),
                can_restart: false,
                paired_server_id: item
                    .pair
                    .as_ref()
                    .map_or_else(SharedString::default, |pair| pair.id.clone().into()),
                paired_server_name: item
                    .pair
                    .as_ref()
                    .map_or_else(SharedString::default, |pair| pair.name.clone().into()),
                pair_start_together: item.pair.as_ref().is_some_and(|pair| pair.start_together),
                // Stamped from the lifecycle, which a snapshot knows nothing about.
                pair_starting: false,
                favorite: item.facts.favorite,
                group_id: group
                    .map_or_else(SharedString::default, |group| group.id.to_string().into()),
                group_name: group
                    .map_or_else(SharedString::default, |group| group.name.as_str().into()),
                group_index,
                group_tone: group.map_or(0, |group| i32::from(group.tone)),
                icon_slot: host_icon_slot(item.icon.as_deref()),
            };
            Listed {
                row,
                facts: item.facts,
            }
        })
        .collect::<Vec<_>>();
    let all_rows = list.replace(ui, listed, groups, sort, order);
    // The list a pending reveal was waiting for. Cleared whether or not the
    // card turned up, so a request cannot outlive the list it was made against.
    let pending = ui.get_pending_reveal_instance();
    if !pending.is_empty() {
        ui.set_pending_reveal_instance(SharedString::default());
        if ui.get_selected_instance_index() >= 0 && ui.get_selected_instance_id() == pending {
            ui.set_reveal_selected_instance(ui.get_reveal_selected_instance().saturating_add(1));
        }
    }
    all_rows
}

/// Fills the server lists, and returns every server's row.
fn apply_server_snapshot(
    ui: &AppWindow,
    servers: Vec<crate::presentation::ServerPresentation>,
) -> Vec<ServerRow> {
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
    // Favorites first, and otherwise the list's own order.
    global_rows.sort_by_key(|row| !row.favorite);
    let global_index = global_selection_index(&global_rows, &previous_global_server);
    if let Some(index) = global_index {
        ui.set_selected_global_server_id(global_rows[index].id.clone());
        ui.set_selected_global_server_index(to_i32(index));
    } else {
        ui.set_selected_global_server_id(SharedString::default());
        ui.set_selected_global_server_index(-1);
    }
    ui.set_global_servers(reconciled(&ui.get_global_servers(), global_rows));
    server_rows
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
        snapshot_deletion_note: item.snapshot_deletion_note.into(),
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
        restart_required: item.restart_required,
        path: item.path.into(),
        installed: item.installed,
        needs_attention: item.needs_attention,
        can_start: item.can_start,
        can_stop: item.can_stop,
        can_restart: item.can_restart,
        can_kill: item.can_kill,
        can_send_command: item.can_send_command,
        console_completion: 0,
        console_delivered: false,
        console_status: if item.can_send_command {
            String::new()
        } else {
            server_tools::console_unavailable(item.state_kind)
        }
        .into(),
        can_connect: item.can_connect,
        connect_reason: item.connect_reason.into(),
        restart_stage: SharedString::default(),
        paired_client_id: item
            .pair
            .as_ref()
            .map_or_else(SharedString::default, |pair| pair.id.clone().into()),
        paired_client_name: item
            .pair
            .as_ref()
            .map_or_else(SharedString::default, |pair| pair.name.clone().into()),
        pair_start_together: item.pair.as_ref().is_some_and(|pair| pair.start_together),
        pair_starting: false,
        favorite: item.favorite,
        icon_slot: host_icon_slot(item.icon.as_deref()),
    }
}

/// A server's stored properties, which its Properties form starts from.
fn apply_server_workspace(workspace: &Workspace, data: ServerWorkspaceData) {
    let Some(server) = backend::parse_server_id(&data.server) else {
        return;
    };
    workspace.properties.borrow_mut().load(
        &data.server,
        server_property_form::StoredProperties {
            values: data.values,
            awaiting_restart: data.awaiting_restart,
            lists_every_key: data.lists_every_key,
        },
    );
    publish_property_form(workspace, &data.server, true);
    workspace.update(InstalledHost::Server(server), |view| {
        view.property_online_mode = data.network.online_mode;
        view.property_access = match data.network.access {
            ephervex_core::NetworkAccess::Localhost => "Localhost",
            ephervex_core::NetworkAccess::Lan => "LAN",
            ephervex_core::NetworkAccess::Custom => "Custom",
        }
        .into();
        view.property_address = data.network.bind_address.unwrap_or_default().into();
        view.property_port = data.network.port.to_string().into();
        view.properties_revision = view.properties_revision.wrapping_add(1);
    });
}

/// Puts a crash report in the view that asked for it, and says how much of it arrived.
fn apply_diagnostic_content(ui: &AppWindow, logs: &mut LogViews, content: &DiagnosticContent) {
    // Through the same expansion a log record gets. A crash report is tab
    // indented on nearly every stack line, and a tab has no glyph to draw.
    logs.apply_report(content);
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

/// The accounts a launch can be made as, for the Launch as menu.
///
/// The default is named like the rest. It is still the one plain Launch uses,
/// and saying so twice would read as two different accounts.
fn account_launch_entries(rows: &[AccountData]) -> Vec<MenuRow> {
    rows.iter()
        .map(|row| MenuRow {
            id: row.id.as_str().into(),
            label: if row.is_default {
                format!("{} ({})", row.name, lookup("gui-default"))
            } else {
                row.name.clone()
            }
            .into(),
            ..MenuRow::default()
        })
        .collect()
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
    ui.set_mod_action_is_pack_owned(data.is_pack_owned);
    ui.set_mod_action_pack_original_version(data.pack_original_version.into());
    if !data.title.is_empty() {
        ui.set_mod_action_name(data.title.into());
    }
    ui.set_mod_versions(model(mod_version_rows(data.versions)));
}

/// Opens the dialog that asks the user to fetch a release themselves.
///
/// The list is the same one the import modal shows for a blocked pack file,
/// and the same folder watcher is already running over it in the backend, so
/// nothing here has to poll or retry: the finished change arrives as an
/// ordinary result.
///
/// Every operation that waits on a release gets its prompt, whichever dialog
/// the window showed before, because the wait has no other way to be seen or
/// ended. A prompt that arrives while another one is on screen waits its turn
/// instead of replacing it, since replacing would leave its wait with no
/// dialog to cancel it from.
fn apply_mod_acquisition(ui: &AppWindow, data: backend::ModAcquisitionData, workspace: &Workspace) {
    let showing_this =
        ui.get_modal() == "mod-acquire" && ui.get_mod_acquisition_id() == data.id.to_string();
    if prompt_on_screen(ui) && !showing_this {
        workspace.acquisition_prompts.borrow_mut().push_back(data);
        return;
    }
    show_mod_acquisition(ui, &data);
}

/// Whether a prompt an operation is waiting on is on screen. Another prompt
/// waits behind it rather than taking its place unanswered.
fn prompt_on_screen(ui: &AppWindow) -> bool {
    (ui.get_modal() == "mod-acquire" && !ui.get_mod_acquisition_id().is_empty())
        || (ui.get_modal() == "mod-stand-in" && !ui.get_mod_stand_in_id().is_empty())
}

/// Shows the next prompt an operation is waiting on, if none is on screen.
fn show_next_prompt(
    ui: &AppWindow,
    acquisitions: &RefCell<std::collections::VecDeque<backend::ModAcquisitionData>>,
    stand_ins: &RefCell<std::collections::VecDeque<backend::ModStandInData>>,
) {
    if prompt_on_screen(ui) {
        return;
    }
    let acquisition = acquisitions.borrow_mut().pop_front();
    if let Some(next) = acquisition {
        show_mod_acquisition(ui, &next);
        return;
    }
    let stand_in = stand_ins.borrow_mut().pop_front();
    if let Some(next) = stand_in {
        show_mod_stand_in(ui, &next);
    }
}

fn apply_mod_stand_in(ui: &AppWindow, data: backend::ModStandInData, workspace: &Workspace) {
    if prompt_on_screen(ui) {
        workspace.stand_in_prompts.borrow_mut().push_back(data);
        return;
    }
    show_mod_stand_in(ui, &data);
}

fn show_mod_stand_in(ui: &AppWindow, data: &backend::ModStandInData) {
    leave_modal(ui);
    ui.set_mod_action_pending(false);
    ui.set_modal_error(SharedString::default());
    ui.set_mod_stand_in_id(data.id.to_string().into());
    ui.set_mod_stand_in_detail(data.detail.as_str().into());
    ui.set_mod_stand_in_note(data.note.as_str().into());
    ui.set_modal("mod-stand-in".into());
}

/// Takes a stand-in question off screen and out of the queue once its
/// operation stopped waiting for the answer.
fn end_mod_stand_in(ui: &AppWindow, id: u64, workspace: &Workspace) {
    workspace
        .stand_in_prompts
        .borrow_mut()
        .retain(|queued| queued.id != id);
    if ui.get_mod_stand_in_id() != id.to_string() {
        return;
    }
    ui.set_mod_stand_in_id(SharedString::default());
    if ui.get_modal() == "mod-stand-in" {
        ui.set_modal(SharedString::default());
        ui.set_modal_error(SharedString::default());
    }
    show_next_prompt(
        ui,
        &workspace.acquisition_prompts,
        &workspace.stand_in_prompts,
    );
}

fn show_mod_acquisition(ui: &AppWindow, data: &backend::ModAcquisitionData) {
    leave_modal(ui);
    ui.set_mod_action_pending(false);
    ui.set_modal_error(SharedString::default());
    ui.set_mod_acquisition_id(data.id.to_string().into());
    ui.set_mod_acquisition_name(data.name.as_str().into());
    ui.set_mod_acquisition_items(model(
        data.items
            .iter()
            .map(|item| BlockedFileRow {
                identity: blocked_identity_key(&item.identity).into(),
                project_line: item.project_line.as_str().into(),
                file_line: item.file_line.as_str().into(),
                browser_url: item.browser_url.as_str().into(),
            })
            .collect::<Vec<_>>(),
    ));
    ui.set_modal("mod-acquire".into());
}

/// Lets go of the dialog on screen the way its own close button would, before
/// something else takes its place: an install confirmation gives its place in
/// the browser's queue back, and an import stops reading its archive. Without
/// this the queue would hold the abandoned request until the app restarts and
/// the import would keep running with nothing to show it.
fn leave_modal(ui: &AppWindow) {
    match ui.get_modal().as_str() {
        "mod-plan" => ui.invoke_abandon_browser_install(),
        "import" => ui.invoke_close_import_modal(),
        "open-pack-link" => ui.invoke_cancel_pack_link(),
        _ => {}
    }
}

/// What a modpack link's download reports to the window.
///
/// Each event names the delivery it is about. One older than the newest the
/// window has seen is ignored, and an archive nothing will import is deleted.
fn apply_pack_link_event(
    ui: &AppWindow,
    event: Event,
    import_flow: &Rc<RefCell<ImportFlowState>>,
    commands: &Sender<Command>,
) {
    match event {
        Event::PackUrlConsent {
            generation,
            url,
            display,
            host,
            redirected,
        } => {
            // The dialog already on screen is the link's own, so it is reused.
            if !pack_link_request_pending(ui, generation)
                || (ui.get_modal() != "open-pack-link" && !make_room_for_dialog(ui))
            {
                return;
            }
            ui.set_pack_link_downloading(false);
            ui.set_pack_link_url(url.as_str().into());
            ui.set_pack_link_display(display.as_str().into());
            ui.set_pack_link_host(host.as_str().into());
            ui.set_pack_link_redirected(redirected);
            ui.set_pack_link_remember(true);
            ui.set_modal_error(SharedString::default());
            ui.set_modal("open-pack-link".into());
        }
        Event::PackUrlDownloading {
            generation,
            url,
            display,
            host,
        } => {
            if !pack_link_request_pending(ui, generation) {
                return;
            }
            let on_screen = ui.get_modal() == "open-pack-link";
            if !on_screen && !make_room_for_dialog(ui) {
                cancel_pack_link_download(ui, commands);
                return;
            }
            // Confirming the question on screen keeps what it showed, so a
            // failure can be retried with the same answer. A different link
            // replaces it.
            if !(on_screen && ui.get_pack_link_url() == url.as_str()) {
                ui.set_pack_link_url(url.as_str().into());
                ui.set_pack_link_display(display.as_str().into());
                ui.set_pack_link_host(host.as_str().into());
                ui.set_pack_link_redirected(false);
                ui.set_pack_link_remember(true);
            }
            ui.set_modal_error(SharedString::default());
            ui.set_modal("open-pack-link".into());
        }
        Event::PackDownloaded {
            generation,
            archive,
        } => {
            // Not wanted once the user canceled or left the dialog, or a
            // newer link took over: nothing will import it, so it goes.
            let wanted = pack_link_request_pending(ui, generation);
            if wanted {
                ui.set_pack_link_downloading(false);
            }
            if wanted && make_room_for_dialog(ui) {
                begin_import(ui, import_flow, commands, archive);
            } else {
                discard_pack_archive(&archive);
            }
        }
        Event::PackUrlEnded {
            generation,
            message,
        } => {
            if !pack_link_request_pending(ui, generation) {
                return;
            }
            ui.set_pack_link_downloading(false);
            match message {
                Some(message) => apply_modal_error(ui, backend::ModalScope::PACK_LINK, message),
                // Canceled from the activity list: the dialog has nothing left
                // to wait for.
                None if ui.get_modal() == "open-pack-link" => {
                    ui.set_modal(SharedString::default());
                }
                None => {}
            }
        }
        _ => {}
    }
}

/// Deletes a downloaded modpack archive that no import will take.
fn discard_pack_archive(archive: &Path) {
    if let Err(error) = std::fs::remove_file(archive) {
        tracing::debug!(%error, "kept a downloaded modpack archive that nothing will import");
    }
}

/// Whether a dialog an event asks for may take the window's place.
///
/// Another operation's wait for a manual download is the one thing on screen
/// that cannot be replaced, because its prompt is the only way to end it. What
/// was asked for cannot wait for it, so it is dropped with a notice and asked
/// for again once the prompt is gone. Anything else on screen is let go of the
/// way its own close button would.
fn make_room_for_dialog(ui: &AppWindow) -> bool {
    if ui.get_modal() == "mod-acquire" || ui.get_modal() == "mod-stand-in" {
        ui.invoke_present_toast(lookup("gui-dialog-not-shown").into(), true);
        return false;
    }
    leave_modal(ui);
    true
}

/// Keeps the reason the background service is down on screen and ends the
/// loading state, because the snapshot that would end it never arrives.
fn apply_backend_unavailable(ui: &AppWindow, message: String) {
    ui.set_startup_error(message.into());
    ui.set_loading(false);
}

/// Shows a failure on the one surface of the one host it is about. Every pane
/// showing that surface of that host reads it; no other host's pane does.
fn apply_host_error(
    workspace: &Workspace,
    host: InstalledHost,
    surface: backend::HostErrorSurface,
    message: String,
) {
    workspace.update(host, |view| match surface {
        backend::HostErrorSurface::Settings => view.runtime_error = message.into(),
        backend::HostErrorSurface::Isolation => view.isolation_error = message.into(),
    });
}

/// Puts a failure where the user can read it: in the dialog it is about while
/// that is still open, as a toast once it is gone.
fn apply_modal_error(ui: &AppWindow, scope: backend::ModalScope, message: String) {
    // A refused mutation leaves its buttons usable again. Without this the
    // modal would show the reason and offer no way to act on it.
    ui.set_mod_action_pending(false);
    if scope.covers(ui.get_modal().as_str()) {
        if scope.names_dialog("loader-change") && ui.get_modal() == "loader-change" {
            ui.set_loader_change_pending(false);
        }
        ui.set_modal_error(message.into());
    } else {
        // What is open now is not the failed operation's to write into.
        ui.invoke_present_toast(message.into(), true);
    }
}

/// Finishes an operation's dialog: closes the one on screen when the operation
/// owns it, and leaves any other alone.
///
/// A dialog the operation does not own was opened by the user since, or is
/// another operation's live wait, and is not the completion's to close.
fn close_modal_for(ui: &AppWindow, scope: backend::ModalScope) {
    // The operation that disabled the modal's actions has answered.
    ui.set_mod_action_pending(false);
    let modal = ui.get_modal();
    if !modal.is_empty() && !scope.covers(modal.as_str()) {
        return;
    }
    leave_modal(ui);
    ui.set_modal(SharedString::default());
    ui.set_modal_error(SharedString::default());
}

/// Closes the prompt whose wait is over, and only that one: a dialog the user
/// has opened since is not the finished wait's to close.
fn end_mod_acquisition(ui: &AppWindow, id: u64, workspace: &Workspace) {
    workspace
        .acquisition_prompts
        .borrow_mut()
        .retain(|queued| queued.id != id);
    if ui.get_mod_acquisition_id() != id.to_string() {
        return;
    }
    ui.set_mod_acquisition_id(SharedString::default());
    if ui.get_modal() == "mod-acquire" {
        ui.set_modal(SharedString::default());
        ui.set_modal_error(SharedString::default());
        ui.set_mod_action_pending(false);
    }
    show_next_prompt(
        ui,
        &workspace.acquisition_prompts,
        &workspace.stand_in_prompts,
    );
}

/// Fills in what the open mod declares and what declares it.
fn apply_mod_relations(ui: &AppWindow, data: &backend::ModRelationsData) {
    if !addresses_open_mod(ui, &data.host, &data.logical_path) {
        return;
    }
    ui.set_mod_action_requires(model(relation_rows(&data.view.requires)));
    ui.set_mod_action_required_by(model(relation_rows(&data.view.required_by)));
    ui.set_mod_action_requires_note(data.view.requires_note.as_str().into());
    ui.set_mod_action_relations_refreshable(data.view.refreshable);
}

fn relation_rows(rows: &[presentation::RelationRowPresentation]) -> Vec<RelationRow> {
    rows.iter()
        .map(|row| RelationRow {
            name: row.name.as_str().into(),
            detail: row.detail.as_str().into(),
            kind: row.kind.as_str().into(),
            state: row.state.as_str().into(),
            attention: row.attention,
        })
        .collect()
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
    workspace: &Workspace,
    results: backend::ModResultsData,
    commands: &Sender<Command>,
) {
    let host = results.host;
    let next = workspace.mods.borrow_mut().apply_results(
        results,
        &workspace.browser.installs.borrow(),
        &workspace.browser.search_requests,
    );
    mod_views::publish(workspace, &workspace.mods.borrow(), host);
    mod_views::send_if(commands, next);
}

/// The largest page a single provider request may take, which is the launcher's
/// budget rather than the user's choice.
fn request_limit() -> u8 {
    ephervex_core::MAX_SEARCH_LIMIT
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
    // A server install also stops for what the catalog never stated or says
    // does not run on a server, because those are the user's decisions.
    if !direct
        || !plan.linked.targets.is_empty()
        || !plan.unstated.is_empty()
        || !plan.client_only.is_empty()
    {
        return PlanDisposition::Confirm;
    }
    if plan.install_count == 0 {
        return PlanDisposition::NothingToDo;
    }
    PlanDisposition::Install
}

/// Puts a plan in the confirmation dialog, addressed to the host it installs on.
fn show_install_confirmation(ui: &AppWindow, plan: backend::ModPlanData) {
    ui.set_mod_action_host(plan.instance.as_str().into());
    ui.set_mod_action_host_kind(if plan.server { "server" } else { "client" }.into());
    ui.set_mod_action_path(SharedString::default());
    ui.set_mod_plan_kind(if plan.server { "server" } else { "client" }.into());
    ui.set_mod_plan_unstated(model(
        plan.unstated
            .iter()
            .map(|(_, name)| SharedString::from(name.as_str()))
            .collect(),
    ));
    ui.set_mod_plan_unstated_ids(model(
        plan.unstated
            .iter()
            .map(|(id, _)| SharedString::from(id.as_str()))
            .collect(),
    ));
    ui.set_mod_plan_client_only(model(
        plan.client_only
            .iter()
            .map(|line| SharedString::from(line.as_str()))
            .collect(),
    ));
    ui.set_mod_plan_restart_note(if plan.server && plan.running {
        lookup("gui-server-mod-plan-restart").into()
    } else {
        SharedString::default()
    });
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
}

fn apply_mod_install_plan(
    ui: &AppWindow,
    plan: backend::ModPlanData,
    workspace: &Workspace,
    commands: &Sender<Command>,
) {
    let browser = &workspace.browser;
    // The plan answers the request the queue is waiting on, or nobody: a
    // request that was abandoned or already ended has no one to show it to.
    // What dialog is open, and for which instance, has no say in that.
    let awaited = BrowserInstall {
        server: plan.server,
        instance: plan.instance.clone(),
        provider: plan.provider.clone(),
        project: plan.requested.clone(),
    };
    let (queued, direct) = {
        let queue = browser.installs.borrow();
        (
            queue.active.as_ref() == Some(&awaited),
            !queue.confirms(&awaited),
        )
    };
    if !queued {
        return;
    }
    let retire = || {
        let next = browser.installs.borrow_mut().finish(&awaited);
        advance_browser_install(commands, workspace, next);
    };

    match plan_disposition(direct, &plan) {
        PlanDisposition::Install => {
            // The queue is retired by this install's own completion, not here:
            // the next one may not start while this one is writing the recipe.
            // Nothing was offered, so no linked server was chosen, whatever
            // rows an open dialog holds.
            let command = if plan.server {
                server_install_command(
                    &plan.instance,
                    &plan.provider,
                    &plan.project,
                    &plan.version_id,
                    Vec::new(),
                    false,
                )
            } else {
                install_command(
                    &plan.instance,
                    &plan.provider,
                    &plan.project,
                    &plan.version_id,
                    Vec::new(),
                    false,
                )
            };
            let Some(command) = command else {
                retire();
                return;
            };
            send(commands, command);
            return;
        }
        PlanDisposition::NothingToDo => {
            ui.invoke_present_toast(lookup("gui-mod-already-present").into(), false);
            retire();
            return;
        }
        // The confirmation owns the request until it is answered or closed.
        PlanDisposition::Confirm => {}
    }
    // A confirmation arriving over a dialog the user opened while it was being
    // planned would replace what they are looking at, and would leave the
    // linked-server rows describing a different operation than the one on
    // screen.
    let modal = ui.get_modal();
    if !modal.is_empty() && modal != "mod-plan" {
        let name = if plan.title.is_empty() {
            plan.project.as_str()
        } else {
            plan.title.as_str()
        };
        ui.invoke_present_toast(
            lookup_args("gui-mod-install-not-shown", [("name", Arg::from(name))]).into(),
            true,
        );
        retire();
        return;
    }
    // No dialog is open here, so the confirmation can take the dialog
    // addressing. Left as an earlier dialog set it, a late answer for that
    // closed dialog would replace this confirmation and strand the queue.
    show_install_confirmation(ui, plan);
    ui.set_modal("mod-plan".into());
    ui.set_modal_error(SharedString::default());
}

/// Shows what one finished operation did to each target that took part.
fn apply_linked_result(ui: &AppWindow, result: backend::LinkedResultData) {
    let owns_modal = {
        let modal = ui.get_modal();
        modal.is_empty() || result.scope.covers(modal.as_str())
    };
    let needs_review =
        presentation::linked_result_needs_review(result.rows.iter().map(|row| row.tone.as_str()));
    let summary = result.summary.clone();
    ui.set_mod_action_pending(false);
    if !owns_modal {
        // The dialog on screen is not this operation's to replace, so the
        // outcome is said in one line instead of the per-target report.
        ui.invoke_present_toast(summary.into(), needs_review);
        return;
    }
    ui.set_linked_result_title(result.title.into());
    ui.set_linked_result_summary(result.summary.into());
    ui.set_linked_results(model(linked_target_rows(result.rows)));
    ui.set_modal_error(SharedString::default());
    // An operation that reached every target it was asked to says so in one
    // line. The per-target report is opened only when a target did not take
    // it, which is the case a reader has to act on.
    leave_modal(ui);
    if needs_review {
        ui.set_modal("mod-result".into());
    } else {
        ui.set_modal(SharedString::default());
        ui.invoke_present_toast(summary.into(), false);
    }
}

/// Shows what a removal would take with it, here and on each linked server.
fn apply_mod_removal_plan(
    ui: &AppWindow,
    data: backend::ModRemovalPlanData,
    workspace: &Workspace,
) {
    // A plan with no mutation behind it names no mod to remove.
    if data.instance.is_empty() {
        return;
    }
    let Some(awaited) = workspace
        .plan_requests
        .admit(ui, PlanKind::Removal, data.request)
    else {
        return;
    };
    // An install confirmation may have taken the addressing since the ask.
    ui.set_mod_action_host(data.instance.as_str().into());
    ui.set_mod_action_host_kind(data.host_kind.as_str().into());
    ui.set_mod_action_path(data.logical_path.as_str().into());
    ui.set_mod_remove_returns_to_manager(matches!(awaited.asked_from, PlanAskedFrom::Manager(_)));
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
fn apply_linked_confirmation(
    ui: &AppWindow,
    request: u64,
    section: backend::LinkedSectionData,
    workspace: &Workspace,
) {
    if workspace
        .plan_requests
        .admit(ui, PlanKind::Apply, request)
        .is_none()
    {
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
            blocked: row.blocked,
            selected: row.selected,
        })
        .collect()
}

/// Whether a background answer is about the mod the user currently has open.
fn addresses_open_mod(ui: &AppWindow, instance: &str, logical_path: &str) -> bool {
    presentation::result_is_current(instance, &ui.get_mod_action_host())
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
            restricted: row.restricted,
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
            server_support: row.server_support.into(),
            blocked_reason: row.blocked_reason.into(),
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
            title: row.title.into(),
            origin: row.origin.into(),
            compatible: row.compatible.into(),
            pinned_by: match row.pinned_by.as_deref() {
                Some("") => SharedString::default(),
                Some(hosts) => {
                    lookup_args("gui-java-pinned-by", [("hosts", Arg::from(hosts))]).into()
                }
                None => lookup("gui-java-pins-unknown").into(),
            },
            pins_known: row.pinned_by.is_some(),
            pinning: row.pinned_by.unwrap_or_default().into(),
        })
        .collect()
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

thread_local! {
    /// Set by a command that found the worker gone, for the next poll to show.
    /// Commands are sent from the window's own thread, which is also the one
    /// that polls.
    static WORKER_STOPPED: Cell<bool> = const { Cell::new(false) };
}

fn send(sender: &Sender<Command>, command: Command) {
    deliver(sender, command);
}

/// Hands a command to the worker, saying whether it was taken.
fn deliver(sender: &Sender<Command>, command: Command) -> bool {
    if sender.send(command).is_err() {
        tracing::error!("the GUI core worker stopped unexpectedly");
        WORKER_STOPPED.set(true);
        return false;
    }
    true
}

/// Starts a settings save unless one of its groups is still saving.
///
/// The groups stay marked as saving until their finish events arrive, which is
/// what keeps their buttons from queuing a second identical command.
fn start_settings_save(
    ui: &slint::Weak<AppWindow>,
    drafts: &Rc<RefCell<SettingsDrafts>>,
    sender: &Sender<Command>,
    groups: &[SettingsSave],
    submit: impl FnOnce(&mut SettingsDrafts),
    command: Command,
) {
    let mut held = drafts.borrow_mut();
    if groups.iter().any(|group| held.is_saving(*group)) {
        return;
    }
    submit(&mut held);
    if !deliver(sender, command) {
        for group in groups {
            held.withdraw(*group);
        }
    }
    if let Some(ui) = ui.upgrade() {
        show_settings_saving(&ui, &held);
    }
}

/// Shows which settings groups have a save under way.
fn show_settings_saving(ui: &AppWindow, drafts: &SettingsDrafts) {
    ui.set_java_saving(drafts.is_saving(SettingsSave::JavaSettings));
    ui.set_storage_root_saving(drafts.is_saving(SettingsSave::StorageRoot));
    ui.set_microsoft_client_id_saving(drafts.is_saving(SettingsSave::MicrosoftClientId));
    ui.set_user_agent_saving(drafts.is_saving(SettingsSave::UserAgent));
    ui.set_artifact_cache_limit_saving(drafts.is_saving(SettingsSave::ArtifactCacheLimit));
}

fn take_worker_stopped() -> bool {
    WORKER_STOPPED.replace(false)
}

/// Shows that the background service is gone, keeping the reason already on
/// screen when it reported one itself.
fn report_worker_stopped(ui: &AppWindow) {
    if ui.get_startup_error().is_empty() {
        apply_backend_unavailable(ui, lookup("gui-background-service-stopped"));
    }
}

fn to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use crate::diagnosis_view::{DiagnosisView, FindingView, OutcomeView, PreviewView};
    use crate::presentation::ActivityPresentation;
    use crate::test_support::{ui_declaration, ui_markup};
    use crate::ui::{MinecraftPreferenceRow, WorkspaceHostChoice};
    use ephervex_core::{ArtifactCacheLimit, DiagnosticFix, FindingClass};
    use ephervex_i18n::has;
    use i_slint_backend_testing::{ElementHandle, ElementQuery};

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

    #[test]
    fn unavailable_backend_ends_loading_and_keeps_its_reason_on_screen() {
        let ui = test_window();
        assert!(ui.get_loading());

        apply_backend_unavailable(&ui, "config.toml is malformed".to_owned());

        assert!(!ui.get_loading());
        assert_eq!(ui.get_startup_error(), "config.toml is malformed");
        assert_eq!(ui.get_modal_error(), "");
    }

    #[test]
    fn a_command_sent_to_a_stopped_worker_puts_the_unavailable_banner_on_screen() {
        let ui = test_window();
        let (commands, received) = std::sync::mpsc::channel();
        drop(received);
        assert!(!take_worker_stopped());

        send(&commands, Command::Refresh);

        assert!(take_worker_stopped());
        assert!(!take_worker_stopped());
        report_worker_stopped(&ui);
        assert!(!ui.get_loading());
        assert_eq!(
            ui.get_startup_error(),
            lookup("gui-background-service-stopped")
        );
    }

    #[test]
    fn a_command_sent_to_a_running_worker_does_not_report_it_stopped() {
        let (commands, received) = std::sync::mpsc::channel();

        send(&commands, Command::Refresh);

        assert!(!take_worker_stopped());
        assert_eq!(received.try_iter().count(), 1);
    }

    #[test]
    fn a_stopped_worker_keeps_the_reason_it_already_reported() {
        let ui = test_window();
        apply_backend_unavailable(&ui, "config.toml is malformed".to_owned());

        report_worker_stopped(&ui);

        assert_eq!(ui.get_startup_error(), "config.toml is malformed");
    }

    fn isolation_data(host: &str, active: bool, phase: &str) -> IsolationData {
        IsolationData {
            host: host.to_owned(),
            host_kind: "client".to_owned(),
            active,
            phase: phase.to_owned(),
            round: usize::from(active),
            granularity: 2,
            enabled: vec!["mods/a.jar".to_owned()],
            disabled: vec!["mods/b.jar".to_owned()],
            candidates: Vec::new(),
            unrecorded: Vec::new(),
            unidentifiable: Vec::new(),
            missing: Vec::new(),
            linked_servers: Vec::new(),
            linked_unrecorded: Vec::new(),
            linked_missing: Vec::new(),
            followed_servers: Vec::new(),
            untested_server_mods: Vec::new(),
            led_by: String::new(),
            reduced_confidence: false,
            confidence: String::new(),
            remaining_candidates: Vec::new(),
            pinned: Vec::new(),
            required_dependencies: Vec::new(),
            history: Vec::new(),
            summary: "Waiting for an answer.".to_owned(),
            recovery: String::new(),
            recovery_detail: String::new(),
            basis: None,
        }
    }

    /// A window with the workspace and isolation actions registered, its
    /// commands, and one client open on its Isolation surface.
    fn isolation_window() -> (
        AppWindow,
        Workspace,
        std::sync::mpsc::Receiver<Command>,
        InstalledHost,
    ) {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        let (sender, isolation_commands) = std::sync::mpsc::channel();
        register_isolation_actions(&ui, &sender, &workspace);
        let host = client_host();
        workspace.open(&ui, host, Surface::Isolation);
        drop(commands);
        (ui, workspace, isolation_commands, host)
    }

    /// The first host view, which every pane on that host reads.
    fn first_view(ui: &AppWindow) -> HostView {
        ui.get_host_views().row_data(0).expect("a host is shown")
    }

    fn candidates(names: &[&str]) -> Vec<backend::IsolationCandidateData> {
        names
            .iter()
            .map(|name| backend::IsolationCandidateData {
                path: format!("mods/{name}.jar"),
                display_path: format!("{name}.jar"),
                name: (*name).to_owned(),
            })
            .collect()
    }

    fn kind_and_id(host: InstalledHost) -> (SharedString, SharedString) {
        (host.kind().into(), host.id().into())
    }

    #[test]
    fn reopening_a_workspace_applies_its_durable_isolation_state() {
        let (ui, workspace, _, host) = isolation_window();

        assert!(workspace.isolation.borrow().accepts(host, 0));
        assert!(apply_isolation_data(
            &workspace,
            isolation_data(&host.id(), true, "round")
        ));

        let view = first_view(&ui);
        assert!(view.isolation_active);
        assert_eq!(view.isolation_phase, "round");
        assert_eq!(view.isolation_round, 1);
        assert_eq!(view.workflow_id, ISOLATION_WORKFLOW);
        assert_eq!(ui.get_modal(), "");
    }

    #[test]
    fn a_new_setup_requires_fresh_acknowledgements_and_an_ended_session_shows_no_phase() {
        let (ui, workspace, _, host) = isolation_window();
        workspace.update(host, |view| {
            view.isolation_accept_unrecorded = true;
            view.isolation_accept_missing = true;
        });

        apply_isolation_data(&workspace, isolation_data(&host.id(), false, "setup"));
        assert!(!first_view(&ui).isolation_accept_unrecorded);
        assert!(!first_view(&ui).isolation_accept_missing);

        apply_isolation_data(&workspace, isolation_data(&host.id(), false, "none"));
        assert_eq!(first_view(&ui).isolation_phase, "none");
        assert!(first_view(&ui).workflow_id.is_empty());
    }

    #[test]
    fn a_kept_isolation_result_stays_on_every_surface_until_closed() {
        let (ui, workspace, _, host) = isolation_window();
        let (kind, id) = kind_and_id(host);
        ui.invoke_show_pane_surface(1, "Mods".into());

        apply_isolation_data(&workspace, isolation_data(&host.id(), false, "result"));
        let view = first_view(&ui);
        assert_eq!(view.workflow_id, ISOLATION_WORKFLOW);
        assert_eq!(view.workflow_title, lookup("gui-isolation-finished"));
        assert_eq!(
            view.workflow_summary,
            lookup("gui-isolation-finished-detail")
        );
        assert_eq!(view.workflow_pane, 0, "no pane shows the Isolation surface");

        ui.invoke_workflow_action(1, kind, id, ISOLATION_WORKFLOW.into(), "dismiss".into());
        let view = first_view(&ui);
        assert!(view.workflow_id.is_empty());
        assert_eq!(view.isolation_phase, "none");
    }

    #[test]
    fn isolation_state_for_a_host_no_pane_shows_lands_nowhere() {
        let (ui, workspace, _, host) = isolation_window();
        let elsewhere = client_host();

        assert!(!apply_isolation_data(
            &workspace,
            isolation_data(&elsewhere.id(), true, "round")
        ));
        assert_eq!(
            first_view(&ui).isolation_phase,
            "",
            "the shown host is untouched"
        );
        assert_eq!(first_view(&ui).id, host.id().as_str());
    }

    /// Two hosts side by side each keep their own isolation: one's session,
    /// setup, and workflow never appear on the other.
    #[test]
    fn two_shown_hosts_keep_their_own_isolation() {
        let (ui, workspace, _, host) = isolation_window();
        let first = workspace.single_pane().expect("one pane");
        workspace.change_pane(&ui, first, |layout| layout.split_beside(first).is_ok());
        let other = server_host();
        ui.invoke_follow_host(2, "server".into(), other.id().into(), "Isolation".into());

        apply_isolation_data(&workspace, isolation_data(&host.id(), true, "round"));
        let mut setup = isolation_data(&other.id(), false, "setup");
        setup.host_kind = "server".to_owned();
        setup.candidates = candidates(&["lithium"]);
        apply_isolation_data(&workspace, setup);

        let views = ui.get_host_views();
        let client_view = views.row_data(0).expect("client");
        let server_view = views.row_data(1).expect("server");
        assert_eq!(client_view.isolation_phase, "round");
        assert_eq!(client_view.workflow_id, ISOLATION_WORKFLOW);
        assert_eq!(server_view.isolation_phase, "setup");
        assert!(
            server_view.workflow_id.is_empty(),
            "a setup is not yet a session"
        );
        assert_eq!(server_view.isolation_candidates.row_count(), 1);
        assert_eq!(client_view.isolation_candidates.row_count(), 0);
    }

    /// The view of `host`, which every pane showing it reads.
    fn view_of(ui: &AppWindow, host: InstalledHost) -> HostView {
        ui.get_host_views()
            .iter()
            .find(|view| view.kind == host.kind() && view.id == host.id().as_str())
            .expect("the host is shown")
    }

    /// A client open on its Isolation surface beside its paired server.
    fn client_beside_its_server() -> (AppWindow, Workspace, InstalledHost, InstalledHost) {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        let (client, server) = (client_host(), server_host());
        let (clients, servers) = paired_rows(client, server, false, false);
        workspace.state.borrow_mut().set_rows(clients, servers);
        workspace.open(&ui, client, Surface::Isolation);
        ui.invoke_split_pane_beside(1);
        ui.invoke_show_linked_pair(1, "server".into(), server.id().into());
        drop(commands);
        (ui, workspace, client, server)
    }

    #[test]
    fn an_error_reported_for_one_host_is_shown_only_on_that_host() {
        let (ui, workspace, client, server) = client_beside_its_server();

        for (surface, read) in [
            (
                backend::HostErrorSurface::Isolation,
                (|view: &HostView| view.isolation_error.to_string()) as fn(&HostView) -> String,
            ),
            (backend::HostErrorSurface::Settings, |view: &HostView| {
                view.runtime_error.to_string()
            }),
        ] {
            apply_host_error(&workspace, client, surface, "Refused".to_owned());

            assert_eq!(read(&view_of(&ui, client)), "Refused");
            assert_eq!(
                read(&view_of(&ui, server)),
                "",
                "the other host's panes never show it"
            );
        }
        assert_eq!(ui.get_modal_error(), "", "no dialog is written into");
    }

    #[test]
    fn an_isolation_reload_for_another_host_keeps_a_hosts_error() {
        let (ui, workspace, client, server) = client_beside_its_server();
        apply_host_error(
            &workspace,
            client,
            backend::HostErrorSurface::Isolation,
            "Refused".to_owned(),
        );
        apply_host_error(
            &workspace,
            server,
            backend::HostErrorSurface::Isolation,
            "Also refused".to_owned(),
        );

        let mut reload = isolation_data(&server.id(), false, "none");
        "server".clone_into(&mut reload.host_kind);
        assert!(apply_isolation_data(&workspace, reload));

        assert_eq!(view_of(&ui, server).isolation_error, "");
        assert_eq!(view_of(&ui, client).isolation_error, "Refused");

        apply_isolation_data(&workspace, isolation_data(&client.id(), false, "none"));
        assert_eq!(view_of(&ui, client).isolation_error, "");
    }

    #[test]
    fn an_isolation_reload_leaves_a_dialogs_error_alone() {
        let (ui, workspace, _, host) = isolation_window();
        ui.set_modal("rename".into());
        ui.set_modal_error("Name taken".into());

        apply_isolation_data(&workspace, isolation_data(&host.id(), false, "none"));

        assert_eq!(ui.get_modal_error(), "Name taken");
    }

    #[test]
    fn a_runtime_save_starts_from_a_form_without_the_last_refusal() {
        let (ui, workspace, _, host) = isolation_window();
        let (sender, commands) = std::sync::mpsc::channel();
        register_host_runtime_actions(&ui, &sender, &workspace);
        let (kind, id) = kind_and_id(host);

        apply_host_error(
            &workspace,
            host,
            backend::HostErrorSurface::Settings,
            "Not a Java".to_owned(),
        );
        ui.invoke_save_host_runtime(
            kind.clone(),
            id.clone(),
            "1024".into(),
            "".into(),
            "".into(),
        );

        assert_eq!(first_view(&ui).runtime_error, "");
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::SaveHostRuntime { .. })
        ));

        apply_host_error(
            &workspace,
            host,
            backend::HostErrorSurface::Settings,
            "Not a Java".to_owned(),
        );
        ui.invoke_clear_host_runtime(kind, id);
        assert_eq!(first_view(&ui).runtime_error, "");
    }

    /// Another pane on the same client switching surfaces or closing leaves
    /// the isolation round the first pane shows exactly as it was.
    #[test]
    fn another_pane_switching_or_closing_leaves_an_isolation_round_alone() {
        let (ui, workspace, isolation_commands, host) = isolation_window();
        apply_isolation_data(&workspace, isolation_data(&host.id(), true, "round"));
        ui.invoke_split_pane_beside(1);
        ui.invoke_show_pane_surface(2, "Logs".into());
        ui.invoke_show_pane_surface(2, "Mods".into());
        ui.invoke_close_pane(2);

        assert_eq!(shown(&workspace), [(host, Surface::Isolation)]);
        let view = first_view(&ui);
        assert_eq!(view.isolation_phase, "round");
        assert_eq!(view.workflow_id, ISOLATION_WORKFLOW);
        assert!(
            isolation_commands.try_recv().is_err(),
            "nothing was asked of the session"
        );
    }

    /// An active round's banner belongs to the pane showing the round in
    /// full; the other panes on that host leave it out, and get it back when
    /// no pane shows the round.
    #[test]
    fn the_isolation_banner_stays_with_the_pane_showing_the_round() {
        let (ui, workspace, _, host) = isolation_window();
        apply_isolation_data(&workspace, isolation_data(&host.id(), true, "round"));
        assert_eq!(first_view(&ui).workflow_pane, 1);

        ui.invoke_split_pane_beside(1);
        ui.invoke_show_pane_surface(2, "Mods".into());
        assert_eq!(
            first_view(&ui).workflow_pane,
            1,
            "the Isolation pane keeps it"
        );

        ui.invoke_show_pane_surface(1, "Logs".into());
        assert_eq!(first_view(&ui).workflow_pane, 0, "no pane shows the round");
        ui.invoke_show_pane_surface(2, "Isolation".into());
        assert_eq!(first_view(&ui).workflow_pane, 2);
    }

    #[test]
    fn a_host_change_without_a_session_retires_the_setup_planned_before_it() {
        let (ui, workspace, _, host) = isolation_window();
        let (kind, id) = kind_and_id(host);
        let mut narrowed = isolation_data(&host.id(), false, "setup");
        narrowed.candidates = candidates(&["e"]);
        apply_isolation_data(&workspace, narrowed);
        ui.invoke_toggle_isolation_pin(kind, id, "mods/e.jar".into(), true);

        // What the worker sends after a restore when no session exists.
        apply_isolation_data(&workspace, isolation_data(&host.id(), false, "none"));
        assert_eq!(first_view(&ui).isolation_phase, "none");
        {
            let flow = workspace.isolation.borrow();
            let setup = flow.setup(host).expect("loaded");
            assert!(setup.candidates.is_empty());
            assert!(setup.pinned.is_empty());
        }

        let mut restored = isolation_data(&host.id(), false, "setup");
        restored.candidates = candidates(&["a", "b", "e"]);
        apply_isolation_data(&workspace, restored);
        let rows = first_view(&ui).isolation_candidates;
        assert_eq!(rows.row_count(), 3);
        assert!(rows.iter().all(|row| !row.selected));
    }

    #[test]
    fn canceling_a_setup_discards_it() {
        let (ui, workspace, commands, host) = isolation_window();
        let (kind, id) = kind_and_id(host);
        let mut setup = isolation_data(&host.id(), false, "setup");
        setup.candidates = candidates(&["a"]);
        apply_isolation_data(&workspace, setup);
        ui.invoke_toggle_isolation_pin(kind.clone(), id.clone(), "mods/a.jar".into(), true);
        let setup_generation = workspace
            .isolation
            .borrow()
            .setup(host)
            .expect("loaded")
            .generation;

        ui.invoke_discard_isolation_setup(kind.clone(), id.clone());
        assert_eq!(first_view(&ui).isolation_phase, "none");
        assert_eq!(first_view(&ui).isolation_candidates.row_count(), 0);
        assert!(!workspace.isolation.borrow().accepts(host, setup_generation));
        assert!(
            workspace
                .isolation
                .borrow()
                .setup(host)
                .is_some_and(|setup| setup.pinned.is_empty())
        );
        ui.invoke_start_isolation(kind, id, false, false, false);
        assert!(
            commands.try_recv().is_err(),
            "a discarded setup cannot start"
        );
    }

    #[test]
    fn a_repeated_answer_cannot_submit_two_answers_for_one_round() {
        let (ui, workspace, commands, host) = isolation_window();
        let (kind, id) = kind_and_id(host);
        apply_isolation_data(&workspace, isolation_data(&host.id(), true, "round"));

        ui.invoke_answer_isolation(kind.clone(), id.clone(), "present".into());
        ui.invoke_answer_isolation(kind.clone(), id.clone(), "present".into());
        ui.invoke_answer_isolation(kind, id, "gone".into());

        let sent: Vec<_> = commands.try_iter().collect();
        assert_eq!(sent.len(), 1, "one round answered once: {sent:?}");
        assert!(matches!(sent[0], Command::AnswerIsolation { .. }));
        assert_eq!(ui.get_host_busy_id(), host.id().as_str());
        assert_eq!(ui.get_host_busy_text(), lookup("gui-isolation-busy-answer"));
    }

    /// The same host in two panes is one session: answering from the second
    /// pane while the first pane's answer is in flight is refused.
    #[test]
    fn two_panes_on_one_host_answer_one_round_once() {
        let (ui, workspace, commands, host) = isolation_window();
        let (kind, id) = kind_and_id(host);
        let first = workspace.single_pane().expect("one pane");
        workspace.change_pane(&ui, first, |layout| layout.split_beside(first).is_ok());
        ui.invoke_show_pane_surface(2, "Logs".into());
        apply_isolation_data(&workspace, isolation_data(&host.id(), true, "round"));
        assert_eq!(ui.get_host_views().row_count(), 1, "one host, one view");

        ui.invoke_workflow_action(
            1,
            kind.clone(),
            id.clone(),
            ISOLATION_WORKFLOW.into(),
            "present".into(),
        );
        ui.invoke_workflow_action(2, kind, id, ISOLATION_WORKFLOW.into(), "gone".into());

        let sent: Vec<_> = commands.try_iter().collect();
        assert_eq!(sent.len(), 1, "{sent:?}");
    }

    #[test]
    fn a_workflow_action_reaches_its_own_host_and_pane() {
        let (ui, workspace, commands, host) = isolation_window();
        let (kind, id) = kind_and_id(host);
        let first = workspace.single_pane().expect("one pane");
        workspace.change_pane(&ui, first, |layout| layout.split_beside(first).is_ok());
        ui.invoke_show_pane_surface(1, "Logs".into());
        apply_isolation_data(&workspace, isolation_data(&host.id(), true, "round"));

        ui.invoke_workflow_action(
            1,
            kind.clone(),
            id.clone(),
            ISOLATION_WORKFLOW.into(),
            "details".into(),
        );
        assert_eq!(
            shown(&workspace),
            [(host, Surface::Isolation), (host, Surface::Overview)],
            "only the pane it was pressed in moves"
        );

        ui.invoke_workflow_action(2, kind, id, "another-workflow".into(), "gone".into());
        assert!(
            commands.try_recv().is_err(),
            "an unknown workflow does nothing"
        );
    }

    #[test]
    fn a_round_that_still_needs_its_mods_accepts_no_feedback_until_recovered() {
        let (ui, workspace, commands, host) = isolation_window();
        let (kind, id) = kind_and_id(host);
        let mut blocked = isolation_data(&host.id(), true, "round");
        blocked.recovery = "running".to_owned();
        apply_isolation_data(&workspace, blocked);

        assert_eq!(first_view(&ui).isolation_recovery, "running");
        let actions: Vec<_> = first_view(&ui)
            .workflow_actions
            .iter()
            .map(|action| action.id.to_string())
            .collect();
        assert_eq!(actions, ["details"]);
        assert_eq!(
            first_view(&ui).workflow_summary,
            lookup("gui-isolation-recovery-summary")
        );
        ui.invoke_answer_isolation(kind.clone(), id.clone(), "present".into());
        ui.invoke_undo_isolation(kind.clone(), id.clone());
        ui.invoke_finish_isolation(kind.clone(), id.clone());
        assert!(
            commands.try_recv().is_err(),
            "no feedback against unapplied mods"
        );

        ui.invoke_retry_isolation_recovery(kind.clone(), id.clone());
        ui.invoke_retry_isolation_recovery(kind.clone(), id.clone());
        let sent: Vec<_> = commands.try_iter().collect();
        assert_eq!(sent.len(), 1, "{sent:?}");
        let Command::RecoverIsolation { request, .. } = sent[0] else {
            panic!("a retry asks the worker to reconcile: {sent:?}");
        };
        assert_eq!(
            ui.get_host_busy_text(),
            lookup("gui-isolation-busy-recover")
        );

        // The host was still running: the worker says so again.
        let mut still_blocked = isolation_data(&host.id(), true, "round");
        still_blocked.recovery = "failed".to_owned();
        still_blocked.recovery_detail = "The jar is missing.".to_owned();
        apply_isolation_data(&workspace, still_blocked);
        assert!(workspace.isolation.borrow_mut().settle(request));
        clear_host_busy(&ui);
        assert_eq!(
            first_view(&ui).isolation_recovery_detail,
            "The jar is missing."
        );
        ui.invoke_answer_isolation(kind.clone(), id.clone(), "present".into());
        assert!(commands.try_recv().is_err());

        // A later retry succeeds, and the round can be answered.
        ui.invoke_retry_isolation_recovery(kind.clone(), id.clone());
        let Ok(Command::RecoverIsolation { request, .. }) = commands.try_recv() else {
            panic!("a failed recovery can be retried");
        };
        apply_isolation_data(&workspace, isolation_data(&host.id(), true, "round"));
        assert!(workspace.isolation.borrow_mut().settle(request));
        clear_host_busy(&ui);
        assert_eq!(first_view(&ui).isolation_recovery, "");
        ui.invoke_retry_isolation_recovery(kind.clone(), id.clone());
        assert!(commands.try_recv().is_err(), "nothing to recover any more");
        ui.invoke_answer_isolation(kind, id, "present".into());
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::AnswerIsolation { .. })
        ));
    }

    #[test]
    fn a_pending_isolation_change_refuses_every_other_change_until_it_settles() {
        let (ui, workspace, commands, host) = isolation_window();
        let (kind, id) = kind_and_id(host);
        apply_isolation_data(&workspace, isolation_data(&host.id(), true, "round"));

        ui.invoke_undo_isolation(kind.clone(), id.clone());
        ui.invoke_undo_isolation(kind.clone(), id.clone());
        ui.invoke_finish_isolation(kind.clone(), id.clone());
        ui.invoke_cancel_isolation(kind.clone(), id.clone());
        ui.invoke_answer_isolation(kind.clone(), id.clone(), "gone".into());
        let sent: Vec<_> = commands.try_iter().collect();
        assert_eq!(sent.len(), 1, "{sent:?}");
        let Command::UndoIsolation { request, .. } = sent[0] else {
            panic!("the first change is the one sent: {sent:?}");
        };

        assert!(
            !workspace
                .isolation
                .borrow_mut()
                .settle(request.wrapping_add(1))
        );
        ui.invoke_cancel_isolation(kind.clone(), id.clone());
        assert!(
            commands.try_recv().is_err(),
            "an unrelated settle releases nothing"
        );

        assert!(workspace.isolation.borrow_mut().settle(request));
        clear_host_busy(&ui);
        assert_eq!(ui.get_host_busy_id(), "");
        ui.invoke_cancel_isolation(kind, id);
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::CancelIsolation { .. })
        ));
    }

    #[test]
    fn a_setup_cannot_be_discarded_while_its_start_is_pending() {
        let (ui, workspace, commands, host) = isolation_window();
        let (kind, id) = kind_and_id(host);
        apply_isolation_data(&workspace, isolation_data(&host.id(), false, "setup"));
        let generation = workspace
            .isolation
            .borrow()
            .setup(host)
            .expect("loaded")
            .generation;

        ui.invoke_start_isolation(kind.clone(), id.clone(), false, false, false);
        ui.invoke_start_isolation(kind.clone(), id.clone(), false, false, false);
        ui.invoke_discard_isolation_setup(kind, id);

        assert_eq!(commands.try_iter().count(), 1);
        assert!(
            workspace.isolation.borrow().accepts(host, generation),
            "the start's result stays wanted"
        );
        assert_eq!(first_view(&ui).isolation_phase, "setup");
        assert_eq!(
            ui.get_host_busy_text(),
            lookup("operation-starting-isolation")
        );
    }

    fn finding(title: &str, problem: bool, fix: Option<DiagnosticFix>) -> FindingView {
        FindingView {
            class: if problem {
                FindingClass::Problem
            } else {
                FindingClass::Customized
            },
            title: title.to_owned(),
            detail: String::new(),
            chain: Vec::new(),
            files: Vec::new(),
            fix_label: if fix.is_some() {
                lookup("gui-diagnosis-review-fix")
            } else {
                String::new()
            },
            fix,
            route: None,
            install: None,
        }
    }

    fn diagnosis(problems: Vec<FindingView>, customized: Vec<FindingView>) -> DiagnosisView {
        DiagnosisView {
            healthy: problems.is_empty(),
            incomplete: false,
            problems,
            customized,
            checks: Vec::new(),
            integrity_checked: true,
            report: "Ephervex diagnostic report".to_owned(),
            crash_report: None,
        }
    }

    fn enable_flywheel() -> DiagnosticFix {
        DiagnosticFix::EnableMods(vec![PathBuf::from("mods/flywheel.jar")])
    }

    /// One problem with a fix, and one customization.
    fn mixed() -> DiagnosisView {
        diagnosis(
            vec![finding(
                "Flywheel is turned off",
                true,
                Some(enable_flywheel()),
            )],
            vec![finding("Custom JVM arguments", false, None)],
        )
    }

    /// A window with the Diagnostics surface opened on one client.
    fn diagnostics_surface() -> (
        AppWindow,
        Workspace,
        std::sync::mpsc::Receiver<Command>,
        InstalledHost,
    ) {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        diagnosis_flow::register(&ui, &workspace.commands, &workspace);
        let host = client_host();
        workspace.open(&ui, host, Surface::Diagnostics);
        assert!(
            commands
                .try_iter()
                .any(|command| matches!(command, Command::PlanDiagnosticBundle(planned) if planned == host)),
            "opening the surface asks what a bundle could carry"
        );
        (ui, workspace, commands, host)
    }

    fn sent_request(commands: &std::sync::mpsc::Receiver<Command>) -> u64 {
        match commands.try_recv() {
            Ok(Command::DiagnoseHost { request, .. } | Command::ApplyFix { request, .. }) => {
                request
            }
            other => panic!("expected diagnostics work, got {other:?}"),
        }
    }

    fn diagnosed(workspace: &Workspace, host: InstalledHost, request: u64, view: DiagnosisView) {
        diagnosis_flow::apply_diagnosed(
            workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            request,
            view,
        );
    }

    fn settled(ui: &AppWindow, workspace: &Workspace, request: u64) {
        diagnosis_flow::settle(
            ui,
            workspace,
            &mut workspace.diagnosis.borrow_mut(),
            request,
        );
    }

    /// Diagnoses the client of [`diagnostics_surface`] and shows `view`.
    fn shown_result(
        view: DiagnosisView,
    ) -> (
        AppWindow,
        Workspace,
        std::sync::mpsc::Receiver<Command>,
        InstalledHost,
    ) {
        let (ui, workspace, commands, host) = diagnostics_surface();
        ui.invoke_diagnose_host("client".into(), host.id().into());
        let request = sent_request(&commands);
        diagnosed(&workspace, host, request, view);
        settled(&ui, &workspace, request);
        (ui, workspace, commands, host)
    }

    #[test]
    fn a_diagnosis_shows_problems_apart_from_valid_customizations() {
        let (ui, workspace, commands, host) = diagnostics_surface();
        assert_eq!(first_view(&ui).diagnosis_state, "unchecked");
        ui.invoke_diagnose_host("client".into(), host.id().into());
        let Ok(Command::DiagnoseHost {
            request, integrity, ..
        }) = commands.try_recv()
        else {
            panic!("a diagnosis");
        };
        assert!(integrity, "a diagnosis the user asks for hashes files");
        assert_eq!(first_view(&ui).diagnosis_state, "working");

        diagnosed(&workspace, host, request, mixed());
        settled(&ui, &workspace, request);

        let view = first_view(&ui);
        assert_eq!(view.diagnosis_state, "result");
        assert!(!view.diagnosis_healthy);
        assert_eq!(
            view.diagnosis_summary,
            lookup_args("gui-diagnosis-problems", [("count", Arg::from(1))]).as_str()
        );
        let problem = view.diagnosis_problems.row_data(0).expect("a problem");
        assert!(problem.problem);
        assert_eq!(problem.key, "p0");
        assert!(!problem.fix_label.is_empty());
        let customized = view
            .diagnosis_customized
            .row_data(0)
            .expect("a customization");
        assert!(!customized.problem, "a customization is never a problem");
        assert!(customized.fix_label.is_empty());
    }

    #[test]
    fn a_host_with_only_customizations_is_healthy() {
        let (ui, _workspace, _commands, _host) = shown_result(diagnosis(
            Vec::new(),
            vec![finding("Custom Java 21.0.8", false, None)],
        ));
        let view = first_view(&ui);
        assert!(view.diagnosis_healthy);
        assert_eq!(
            view.diagnosis_summary,
            lookup("gui-diagnosis-healthy").as_str()
        );
        assert_eq!(view.diagnosis_customized.row_count(), 1);
    }

    #[test]
    fn a_failed_diagnosis_is_not_mistaken_for_one_that_found_problems() {
        let (ui, workspace, commands, host) = shown_result(mixed());
        ui.invoke_diagnose_host("client".into(), host.id().into());
        let request = sent_request(&commands);
        diagnosis_flow::apply_diagnosis_failed(
            &workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            request,
            "The version metadata could not be read.".to_owned(),
            false,
        );
        settled(&ui, &workspace, request);

        let view = first_view(&ui);
        assert_eq!(view.diagnosis_state, "failed");
        assert_eq!(
            view.diagnosis_error,
            "The version metadata could not be read."
        );
        assert!(
            !view.diagnosis_summary.is_empty(),
            "the earlier result stays readable under the failure"
        );
    }

    #[test]
    fn a_stopped_diagnosis_says_it_stopped_and_that_nothing_changed() {
        let (ui, workspace, commands, host) = diagnostics_surface();
        ui.invoke_diagnose_host("client".into(), host.id().into());
        let request = sent_request(&commands);
        diagnosis_flow::apply_diagnosis_failed(
            &workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            request,
            String::new(),
            true,
        );
        settled(&ui, &workspace, request);
        assert_eq!(first_view(&ui).diagnosis_state, "canceled");
        assert_eq!(
            first_view(&ui).diagnosis_error,
            lookup("gui-diagnosis-canceled").as_str()
        );
    }

    #[test]
    fn a_diagnosis_lands_only_on_its_own_host() {
        let (ui, workspace, commands, first) = diagnostics_surface();
        let pane = workspace.single_pane().expect("one pane");
        workspace.change_pane(&ui, pane, |layout| layout.split_beside(pane).is_ok());
        let second = server_host();
        ui.invoke_follow_host(2, "server".into(), second.id().into(), "Diagnostics".into());
        commands.try_iter().for_each(drop);
        ui.invoke_diagnose_host("client".into(), first.id().into());
        let request = sent_request(&commands);

        diagnosed(&workspace, first, request, mixed());
        let views = ui.get_host_views();
        let client_view = views.row_data(0).expect("client");
        let server_view = views.row_data(1).expect("server");
        assert_eq!(client_view.diagnosis_problems.row_count(), 1);
        assert_eq!(server_view.diagnosis_state, "unchecked");
        assert_eq!(server_view.diagnosis_problems.row_count(), 0);
    }

    #[test]
    fn a_late_diagnosis_of_a_host_that_left_is_not_shown() {
        let (ui, workspace, commands, first) = diagnostics_surface();
        ui.invoke_diagnose_host("client".into(), first.id().into());
        let request = sent_request(&commands);
        workspace.open(&ui, server_host(), Surface::Diagnostics);

        diagnosed(&workspace, first, request, mixed());
        assert_eq!(first_view(&ui).diagnosis_problems.row_count(), 0);

        workspace.open(&ui, first, Surface::Diagnostics);
        assert_eq!(
            first_view(&ui).diagnosis_state,
            "working",
            "coming back finds the diagnosis still running"
        );
        assert_eq!(first_view(&ui).diagnosis_problems.row_count(), 0);
    }

    #[test]
    fn one_diagnosis_or_fix_runs_at_a_time_whichever_pane_asks() {
        let (ui, workspace, commands, host) = shown_result(mixed());
        let pane = workspace.single_pane().expect("one pane");
        workspace.change_pane(&ui, pane, |layout| layout.split_beside(pane).is_ok());
        ui.invoke_follow_host(2, "client".into(), host.id().into(), "Diagnostics".into());
        commands.try_iter().for_each(drop);

        ui.invoke_diagnose_host("client".into(), host.id().into());
        ui.invoke_diagnose_host("client".into(), host.id().into());
        let request = sent_request(&commands);
        assert!(commands.try_recv().is_err(), "a second request waits");

        settled(&ui, &workspace, request);
        assert_eq!(
            first_view(&ui).diagnosis_state,
            "result",
            "a diagnosis that ended without a result leaves the earlier one"
        );
        ui.invoke_diagnose_host("client".into(), host.id().into());
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::DiagnoseHost { .. })
        ));
    }

    /// Opens the preview of the result's first problem and answers it.
    fn ready_preview(
        ui: &AppWindow,
        workspace: &Workspace,
        commands: &std::sync::mpsc::Receiver<Command>,
        host: InstalledHost,
    ) {
        ui.invoke_review_fix("client".into(), host.id().into(), "p0".into());
        let Ok(Command::PreviewFix { request, fix, .. }) = commands.try_recv() else {
            panic!("a preview");
        };
        assert_eq!(fix, enable_flywheel());
        diagnosis_flow::apply_fix_previewed(
            workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            request,
            Ok(PreviewView {
                changes: vec![crate::diagnosis_view::ChangeView {
                    action: lookup("gui-fix-enable"),
                    name: "Flywheel".to_owned(),
                    detail: "mods/flywheel.jar".to_owned(),
                }],
                notes: vec![lookup("gui-fix-enable-note")],
                blocked: String::new(),
            }),
        );
    }

    #[test]
    fn choosing_a_java_runtime_opens_its_review_before_diagnostics_was_ever_opened() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        java_runtime::register(&ui, &workspace.commands, &workspace);
        let host = client_host();
        workspace.open(&ui, host, Surface::Settings);
        commands.try_iter().for_each(drop);

        ui.invoke_choose_host_java(
            "client".into(),
            host.id().into(),
            "/opt/jdk-21/bin/java".into(),
        );

        let Ok(Command::PreviewFix { fix, .. }) = commands.try_recv() else {
            panic!("a preview of the chosen runtime");
        };
        assert_eq!(
            fix,
            DiagnosticFix::PinJava(PathBuf::from("/opt/jdk-21/bin/java"))
        );
        assert_eq!(first_view(&ui).fix_state, "loading");
        assert!(
            commands.try_recv().is_err(),
            "nothing is applied before the preview is reviewed"
        );

        java_runtime::apply_installed(
            &workspace,
            host,
            Some(PathBuf::from("/data/java/8/jdk8u312/bin/java")),
        );
        let previews: Vec<DiagnosticFix> = commands
            .try_iter()
            .filter_map(|command| match command {
                Command::PreviewFix { fix, .. } => Some(fix),
                _ => None,
            })
            .collect();
        assert_eq!(
            previews,
            [DiagnosticFix::PinJava(PathBuf::from(
                "/data/java/8/jdk8u312/bin/java"
            ))],
            "the pin an install offers is put up for review, never applied"
        );
    }

    #[test]
    fn a_failed_java_read_leaves_nothing_of_the_earlier_one_on_screen() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        let host = client_host();
        workspace.open(&ui, host, Surface::Settings);
        let status = crate::java_view::JavaStatusView {
            tone: "healthy".to_owned(),
            tone_label: "Healthy".to_owned(),
            runtime: "Java 21.0.8, Eclipse Adoptium, amd64".to_owned(),
            summary: "Java 21.0.8 (Eclipse Adoptium)".to_owned(),
            origin: "chosen automatically".to_owned(),
            expected: "Java 21".to_owned(),
            ..crate::java_view::JavaStatusView::default()
        };
        java_runtime::apply_host_java(
            &workspace,
            &backend::HostJavaData {
                host,
                status: Ok(status),
                choices: None,
            },
        );
        assert_eq!(
            first_view(&ui).java_summary,
            "Java 21.0.8 (Eclipse Adoptium)"
        );

        java_runtime::apply_host_java(
            &workspace,
            &backend::HostJavaData {
                host,
                status: Err("The Java check could not run".to_owned()),
                choices: None,
            },
        );

        let view = first_view(&ui);
        assert_eq!(view.java_error, "The Java check could not run");
        for field in [
            view.java_summary,
            view.java_tone,
            view.java_tone_label,
            view.java_runtime,
            view.java_origin,
        ] {
            assert_eq!(field, "", "a stale claim stayed on screen");
        }
    }

    #[test]
    fn a_fix_is_applied_only_from_its_preview_and_only_once() {
        let (ui, workspace, commands, host) = shown_result(mixed());
        ui.invoke_review_fix("client".into(), host.id().into(), "p0".into());
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::PreviewFix { .. })
        ));
        assert_eq!(first_view(&ui).fix_state, "loading");
        ui.invoke_apply_fix("client".into(), host.id().into());
        assert!(
            commands.try_recv().is_err(),
            "nothing is applied before its preview is on screen"
        );
        ui.invoke_cancel_fix("client".into(), host.id().into());
        assert_eq!(first_view(&ui).fix_state, "");

        ready_preview(&ui, &workspace, &commands, host);
        assert_eq!(first_view(&ui).fix_state, "ready");
        assert_eq!(first_view(&ui).fix_changes.row_count(), 1);

        ui.invoke_apply_fix("client".into(), host.id().into());
        ui.invoke_apply_fix("client".into(), host.id().into());
        let Ok(Command::ApplyFix { fix, integrity, .. }) = commands.try_recv() else {
            panic!("an apply");
        };
        assert_eq!(fix, enable_flywheel());
        assert!(integrity, "the diagnosis after it checks what this one did");
        assert!(commands.try_recv().is_err(), "a second apply waits");
        assert_eq!(first_view(&ui).fix_state, "applying");
        assert_eq!(ui.get_host_busy_id(), host.id().as_str());

        ui.invoke_cancel_fix("client".into(), host.id().into());
        assert_eq!(
            first_view(&ui).fix_state,
            "applying",
            "a fix already applying cannot be closed"
        );
    }

    #[test]
    fn a_late_preview_of_a_review_that_was_closed_is_ignored() {
        let (ui, workspace, commands, host) = shown_result(mixed());
        ui.invoke_review_fix("client".into(), host.id().into(), "p0".into());
        let Ok(Command::PreviewFix { request: first, .. }) = commands.try_recv() else {
            panic!("a preview");
        };
        ui.invoke_cancel_fix("client".into(), host.id().into());
        ui.invoke_review_fix("client".into(), host.id().into(), "p0".into());
        diagnosis_flow::apply_fix_previewed(
            &workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            first,
            Err("gone".to_owned()),
        );
        assert_eq!(first_view(&ui).fix_state, "loading");
    }

    #[test]
    fn an_applied_fix_leaves_the_result_stale_until_the_diagnosis_after_it() {
        let (ui, workspace, commands, host) = shown_result(mixed());
        ready_preview(&ui, &workspace, &commands, host);
        ui.invoke_apply_fix("client".into(), host.id().into());
        let request = sent_request(&commands);

        diagnosis_flow::apply_fix_applied(
            &workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            request,
            OutcomeView {
                message: lookup_args("gui-fix-enabled", [("count", Arg::from(1))]),
                failed: false,
                changed: true,
                lines: Vec::new(),
            },
        );
        assert_eq!(first_view(&ui).fix_state, "");
        assert!(first_view(&ui).diagnosis_stale);
        assert!(!first_view(&ui).fix_result.is_empty());

        diagnosed(&workspace, host, request, diagnosis(Vec::new(), Vec::new()));
        settled(&ui, &workspace, request);
        let view = first_view(&ui);
        assert!(!view.diagnosis_stale);
        assert!(view.diagnosis_healthy, "no problem survives the fix");
        assert!(!view.fix_result.is_empty(), "what the fix did stays named");
        assert_eq!(ui.get_host_busy_id(), "");
    }

    #[test]
    fn an_apply_waits_for_a_change_already_running_and_holds_isolation_back() {
        let (ui, workspace, commands, host) = shown_result(mixed());
        ready_preview(&ui, &workspace, &commands, host);
        ui.set_host_busy_kind("client".into());
        ui.set_host_busy_id("elsewhere".into());
        ui.invoke_apply_fix("client".into(), host.id().into());
        assert!(
            commands.try_recv().is_err(),
            "an isolation change owns the host state"
        );
        assert_eq!(first_view(&ui).fix_state, "ready");

        clear_host_busy(&ui);
        ui.invoke_apply_fix("client".into(), host.id().into());
        let request = sent_request(&commands);
        assert_eq!(
            ui.get_host_busy_text(),
            lookup("operation-applying-fix").as_str()
        );

        let (isolation_sender, isolation_commands) = std::sync::mpsc::channel();
        register_isolation_actions(&ui, &isolation_sender, &workspace);
        workspace.open(&ui, host, Surface::Isolation);
        apply_isolation_data(&workspace, isolation_data(&host.id(), false, "setup"));
        ui.invoke_start_isolation("client".into(), host.id().into(), false, false, false);
        assert!(isolation_commands.try_recv().is_err());

        settled(&ui, &workspace, request);
        assert_eq!(ui.get_host_busy_id(), "");
    }

    #[test]
    fn a_failed_run_explains_itself_beside_the_host_without_opening_anything() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        diagnosis_flow::register(&ui, &workspace.commands, &workspace);
        let host = client_host();
        workspace.open(&ui, host, Surface::Overview);
        let mut view = mixed();
        view.crash_report = Some("crash-2026-09-27_12.00.00-client.txt".to_owned());

        diagnosis_flow::apply_run_failed(
            &workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            lookup("gui-run-exited-with-error"),
            None,
            Some(view),
        );

        assert_eq!(
            ui.get_workspace_panes()
                .row_data(0)
                .expect("the pane")
                .surface,
            "Overview",
            "nothing navigates"
        );
        let shown = first_view(&ui);
        assert_eq!(shown.run_failure_title, lookup("gui-run-crashed").as_str());
        assert!(shown.run_likely_cause.contains("Flywheel is turned off"));
        assert_eq!(
            shown.run_crash_report,
            "crash-2026-09-27_12.00.00-client.txt"
        );
        assert_eq!(shown.diagnosis_state, "result");
        assert_eq!(
            shown.diagnosis_failure,
            lookup("gui-run-exited-with-error").as_str(),
            "Diagnostics opens on the failure it was made for"
        );

        ui.invoke_dismiss_run_failure("client".into(), host.id().into());
        assert_eq!(first_view(&ui).run_failure_title, "");
    }

    #[test]
    fn a_failed_run_offers_verification_only_when_its_failure_names_a_remedy() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        diagnosis_flow::register(&ui, &workspace.commands, &workspace);
        let host = client_host();
        workspace.open(&ui, host, Surface::Overview);

        for (remedy, offered) in [
            (None, false),
            (Some(ephervex_core::Remedy::VerifyHost), true),
        ] {
            diagnosis_flow::apply_run_failed(
                &workspace,
                &mut workspace.diagnosis.borrow_mut(),
                host,
                lookup("gui-run-exited-with-error"),
                remedy,
                None,
            );
            assert_eq!(first_view(&ui).run_verify_suggested, offered);
            ui.invoke_dismiss_run_failure("client".into(), host.id().into());
            assert!(!first_view(&ui).run_verify_suggested);
        }
    }

    #[test]
    fn diagnosing_is_refused_while_another_change_rewrites_the_host() {
        let (ui, _workspace, commands, host) = diagnostics_surface();

        ui.set_host_busy_id(host.id().into());
        ui.invoke_diagnose_host("client".into(), host.id().into());
        assert!(commands.try_recv().is_err());

        ui.set_host_busy_id("elsewhere".into());
        ui.invoke_diagnose_host("client".into(), host.id().into());
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::DiagnoseHost { .. })
        ));
    }

    #[test]
    fn a_failed_run_does_not_replace_a_diagnosis_the_user_started() {
        let (ui, workspace, commands, host) = diagnostics_surface();
        ui.invoke_diagnose_host("client".into(), host.id().into());
        let request = sent_request(&commands);
        diagnosis_flow::apply_run_failed(
            &workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            lookup("gui-run-exited-with-error"),
            None,
            Some(mixed()),
        );
        assert_eq!(first_view(&ui).diagnosis_state, "working");
        assert!(!first_view(&ui).run_failure_title.is_empty());

        diagnosed(&workspace, host, request, diagnosis(Vec::new(), Vec::new()));
        assert_eq!(first_view(&ui).diagnosis_failure, "");
        assert!(first_view(&ui).diagnosis_healthy);
    }

    fn bundle_part(name: &str, kind: &'static str) -> backend::BundlePartData {
        backend::BundlePartData {
            name: name.to_owned(),
            kind,
            size_bytes: 2048,
        }
    }

    /// Diagnostics opened on a client, with the planner's usual offer.
    fn planned_bundle() -> (
        AppWindow,
        Workspace,
        std::sync::mpsc::Receiver<Command>,
        InstalledHost,
    ) {
        let (ui, workspace, commands, host) = diagnostics_surface();
        diagnosis_flow::apply_bundle_plan(
            &workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            vec![
                bundle_part("logs/latest.log", "game_log"),
                bundle_part("logs/2026-01-01-1.log.gz", "game_log"),
                bundle_part("crash-reports/crash-new.txt", "crash_report"),
                bundle_part("crash-reports/crash-old.txt", "crash_report"),
                bundle_part("instance/recipe.json", "recipe"),
                bundle_part("instance/instance.json", "record"),
            ],
        );
        (ui, workspace, commands, host)
    }

    fn chosen_rows(ui: &AppWindow) -> Vec<String> {
        first_view(ui)
            .bundle_parts
            .iter()
            .filter(|row| row.selected)
            .map(|row| row.name.to_string())
            .collect()
    }

    #[test]
    fn a_bundle_plan_becomes_rows_with_only_the_newest_files_chosen() {
        let (ui, _workspace, _commands, _host) = planned_bundle();
        assert_eq!(
            first_view(&ui).bundle_parts.row_count(),
            6,
            "every offered part is a row"
        );
        assert_eq!(
            chosen_rows(&ui),
            [
                "logs/latest.log",
                "crash-reports/crash-new.txt",
                "instance/recipe.json",
                "instance/instance.json",
            ],
            "older logs and reports are offered, not taken"
        );
        assert_eq!(first_view(&ui).bundle_selected_count, 4);
        assert_eq!(
            first_view(&ui).bundle_parts.row_data(0).expect("row").kind,
            lookup("gui-bundle-kind-game-log").as_str()
        );
    }

    #[test]
    fn the_chosen_parts_are_what_the_export_asks_for() {
        let (ui, workspace, commands, host) = planned_bundle();
        ui.invoke_toggle_bundle_part(
            "client".into(),
            host.id().into(),
            "instance/recipe.json".into(),
            false,
        );
        ui.invoke_toggle_bundle_part(
            "client".into(),
            host.id().into(),
            "logs/2026-01-01-1.log.gz".into(),
            true,
        );
        ui.invoke_export_bundle("client".into(), host.id().into());
        ui.invoke_export_bundle("client".into(), host.id().into());

        let sent: Vec<_> = commands.try_iter().collect();
        assert_eq!(sent.len(), 1, "a second export waits: {sent:?}");
        let Command::ExportDiagnosticBundle { request, names, .. } = &sent[0] else {
            panic!("an export: {sent:?}");
        };
        assert_eq!(
            names,
            &[
                "logs/latest.log",
                "logs/2026-01-01-1.log.gz",
                "crash-reports/crash-new.txt",
                "instance/instance.json",
            ]
        );
        assert!(first_view(&ui).bundle_exporting);
        ui.invoke_toggle_bundle_part(
            "client".into(),
            host.id().into(),
            "instance/recipe.json".into(),
            true,
        );
        assert_eq!(
            first_view(&ui).bundle_selected_count,
            4,
            "the choice is fixed while it is written"
        );

        diagnosis_flow::settle_bundle(&workspace, &mut workspace.diagnosis.borrow_mut(), *request);
        assert!(!first_view(&ui).bundle_exporting);
    }

    fn exported_report() -> backend::BundleExportData {
        backend::BundleExportData {
            path: PathBuf::from("/home/user/bundle.zip"),
            included: 1,
            unreadable: Vec::new(),
            bytes: 2048,
        }
    }

    /// The Settings Diagnostics page with the launcher's usual offer.
    fn planned_launcher_bundle() -> (AppWindow, Workspace, std::sync::mpsc::Receiver<Command>) {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        diagnosis_flow::register(&ui, &workspace.commands, &workspace);
        ui.invoke_request_launcher_bundle();
        assert!(
            matches!(commands.try_recv(), Ok(Command::PlanLauncherBundle)),
            "opening the page asks what the launcher's own bundle could carry"
        );
        diagnosis_flow::apply_launcher_bundle_plan(
            &ui,
            &mut workspace.diagnosis.borrow_mut(),
            vec![
                bundle_part("launcher/ephervex.2026-01-02.log", "launcher_log"),
                bundle_part("launcher/ephervex.2026-01-01.log", "launcher_log"),
            ],
        );
        (ui, workspace, commands)
    }

    #[test]
    fn the_launcher_bundle_starts_with_only_the_newest_launcher_log_chosen() {
        let (ui, _workspace, _commands) = planned_launcher_bundle();
        assert_eq!(ui.get_launcher_bundle_selected_count(), 1);
        let rows: Vec<_> = ui.get_launcher_bundle_parts().iter().collect();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].selected);
        assert!(!rows[1].selected);
    }

    #[test]
    fn a_launcher_bundle_export_carries_the_chosen_logs_and_waits_for_the_running_one() {
        let (ui, workspace, commands) = planned_launcher_bundle();
        ui.invoke_toggle_launcher_bundle_part("launcher/ephervex.2026-01-01.log".into(), true);
        ui.invoke_export_launcher_bundle();
        ui.invoke_export_launcher_bundle();

        let sent: Vec<_> = commands.try_iter().collect();
        assert_eq!(sent.len(), 1, "a second export waits: {sent:?}");
        let Command::ExportLauncherBundle { request, names } = &sent[0] else {
            panic!("a launcher export: {sent:?}");
        };
        assert_eq!(
            names,
            &[
                "launcher/ephervex.2026-01-02.log",
                "launcher/ephervex.2026-01-01.log"
            ]
        );
        assert!(ui.get_launcher_bundle_exporting());
        ui.invoke_select_all_launcher_bundle_parts(false);
        assert_eq!(
            ui.get_launcher_bundle_selected_count(),
            2,
            "the choice is fixed while it is written"
        );

        diagnosis_flow::settle_launcher_bundle(
            &ui,
            &mut workspace.diagnosis.borrow_mut(),
            *request,
        );
        assert!(!ui.get_launcher_bundle_exporting());
    }

    #[test]
    fn an_empty_launcher_bundle_choice_is_not_exported() {
        let (ui, _workspace, commands) = planned_launcher_bundle();
        ui.invoke_select_all_launcher_bundle_parts(false);
        ui.invoke_export_launcher_bundle();
        assert!(commands.try_recv().is_err());
        assert!(!ui.get_launcher_bundle_exporting());
    }

    #[test]
    fn a_launcher_export_result_answers_only_the_newest_launcher_export() {
        let (ui, workspace, commands) = planned_launcher_bundle();
        ui.invoke_export_launcher_bundle();
        let Ok(Command::ExportLauncherBundle { request, .. }) = commands.try_recv() else {
            panic!("a launcher export");
        };

        diagnosis_flow::apply_launcher_bundle_exported(
            &ui,
            &workspace.diagnosis.borrow(),
            request + 1,
            &exported_report(),
        );
        assert!(ui.get_launcher_bundle_result().is_empty());

        diagnosis_flow::apply_launcher_bundle_exported(
            &ui,
            &workspace.diagnosis.borrow(),
            request,
            &exported_report(),
        );
        assert!(
            ui.get_launcher_bundle_result()
                .contains("/home/user/bundle.zip")
        );
        assert_eq!(
            ui.get_launcher_bundle_result_path(),
            "/home/user/bundle.zip"
        );

        diagnosis_flow::apply_launcher_bundle_export_failed(
            &ui,
            &workspace.diagnosis.borrow(),
            request,
            "no space",
        );
        assert!(ui.get_launcher_bundle_result().is_empty());
        assert!(ui.get_launcher_bundle_result_path().is_empty());
        assert_eq!(ui.get_launcher_bundle_error(), "no space");
    }

    #[test]
    fn settling_a_host_export_leaves_the_launcher_export_pending() {
        let (ui, workspace, commands) = planned_launcher_bundle();
        ui.invoke_export_launcher_bundle();
        let Ok(Command::ExportLauncherBundle { request, .. }) = commands.try_recv() else {
            panic!("a launcher export");
        };

        diagnosis_flow::settle_launcher_bundle(
            &ui,
            &mut workspace.diagnosis.borrow_mut(),
            request + 1,
        );
        assert!(ui.get_launcher_bundle_exporting());
    }

    #[test]
    fn settling_one_hosts_export_leaves_another_hosts_export_pending() {
        let (ui, workspace, commands, first) = planned_bundle();
        let pane = workspace.single_pane().expect("one pane");
        workspace.change_pane(&ui, pane, |layout| layout.split_beside(pane).is_ok());
        let second = server_host();
        ui.invoke_follow_host(2, "server".into(), second.id().into(), "Diagnostics".into());
        diagnosis_flow::apply_bundle_plan(
            &workspace,
            &mut workspace.diagnosis.borrow_mut(),
            second,
            vec![bundle_part("instance/instance.json", "record")],
        );
        commands.try_iter().for_each(drop);

        ui.invoke_export_bundle("client".into(), first.id().into());
        ui.invoke_export_bundle("server".into(), second.id().into());
        let requests: Vec<u64> = commands
            .try_iter()
            .map(|command| match command {
                Command::ExportDiagnosticBundle { request, .. } => request,
                other => panic!("an export: {other:?}"),
            })
            .collect();
        assert_eq!(requests.len(), 2);
        assert_ne!(requests[0], requests[1], "each export has its own id");

        diagnosis_flow::settle_bundle(
            &workspace,
            &mut workspace.diagnosis.borrow_mut(),
            requests[0],
        );
        let views = ui.get_host_views();
        assert!(!views.row_data(0).expect("client").bundle_exporting);
        assert!(
            views.row_data(1).expect("server").bundle_exporting,
            "the other host is still writing"
        );

        ui.invoke_export_bundle("server".into(), second.id().into());
        assert!(
            commands.try_recv().is_err(),
            "the other host's export still waits for its own result"
        );
    }

    #[test]
    fn a_late_export_result_from_before_a_host_left_is_not_taken_as_its_new_answer() {
        let (ui, workspace, commands, host) = planned_bundle();
        ui.invoke_export_bundle("client".into(), host.id().into());
        let Ok(Command::ExportDiagnosticBundle {
            request: earlier, ..
        }) = commands.try_recv()
        else {
            panic!("an export");
        };

        workspace.diagnosis.borrow_mut().follow(&[]);
        diagnosis_flow::open(&workspace, &workspace.commands, host);
        diagnosis_flow::apply_bundle_plan(
            &workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            vec![bundle_part("instance/instance.json", "record")],
        );
        ui.invoke_export_bundle("client".into(), host.id().into());
        let sent: Vec<_> = commands
            .try_iter()
            .filter(|command| matches!(command, Command::ExportDiagnosticBundle { .. }))
            .collect();
        let [Command::ExportDiagnosticBundle { request: later, .. }] = sent.as_slice() else {
            panic!("one export: {sent:?}");
        };
        assert_ne!(*later, earlier, "a returning host does not restart its ids");

        diagnosis_flow::apply_bundle_exported(
            &workspace,
            &workspace.diagnosis.borrow(),
            host,
            earlier,
            &exported_report(),
        );
        assert!(first_view(&ui).bundle_result.is_empty());

        diagnosis_flow::apply_bundle_exported(
            &workspace,
            &workspace.diagnosis.borrow(),
            host,
            *later,
            &exported_report(),
        );
        assert!(!first_view(&ui).bundle_result.is_empty());
    }

    #[test]
    fn an_empty_bundle_choice_is_never_exported() {
        let (ui, _workspace, commands, host) = planned_bundle();
        ui.invoke_select_all_bundle_parts("client".into(), host.id().into(), false);
        assert_eq!(first_view(&ui).bundle_selected_count, 0);
        ui.invoke_export_bundle("client".into(), host.id().into());
        assert!(commands.try_recv().is_err());
        assert!(!first_view(&ui).bundle_exporting);

        ui.invoke_select_all_bundle_parts("client".into(), host.id().into(), true);
        assert_eq!(first_view(&ui).bundle_selected_count, 6);
    }

    #[test]
    fn a_new_plan_of_the_same_host_keeps_the_users_choice() {
        let (ui, workspace, _commands, host) = planned_bundle();
        ui.invoke_toggle_bundle_part(
            "client".into(),
            host.id().into(),
            "logs/latest.log".into(),
            false,
        );
        diagnosis_flow::apply_bundle_plan(
            &workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            vec![
                bundle_part("logs/latest.log", "game_log"),
                bundle_part("instance/recipe.json", "recipe"),
            ],
        );
        assert_eq!(chosen_rows(&ui), ["instance/recipe.json"]);
    }

    #[test]
    fn an_export_outcome_is_shown_only_for_its_own_host() {
        let (ui, workspace, commands, host) = planned_bundle();
        ui.invoke_export_bundle("client".into(), host.id().into());
        let Ok(Command::ExportDiagnosticBundle { request, .. }) = commands.try_recv() else {
            panic!("an export");
        };
        let report = backend::BundleExportData {
            path: PathBuf::from("/home/user/bundle.zip"),
            included: 4,
            unreadable: vec!["logs/latest.log".to_owned()],
            bytes: 4096,
        };
        diagnosis_flow::apply_bundle_exported(
            &workspace,
            &workspace.diagnosis.borrow(),
            host,
            request,
            &report,
        );
        assert_eq!(first_view(&ui).bundle_result_path, "/home/user/bundle.zip");
        assert!(
            first_view(&ui)
                .bundle_result
                .contains("/home/user/bundle.zip")
        );
        assert!(
            first_view(&ui).bundle_result.contains("logs/latest.log"),
            "what could not be read is named"
        );

        diagnosis_flow::apply_bundle_export_failed(
            &workspace,
            &workspace.diagnosis.borrow(),
            host,
            request,
            "The disk is full.",
        );
        assert_eq!(first_view(&ui).bundle_error, "The disk is full.");
        assert_eq!(first_view(&ui).bundle_result, "");

        workspace.open(&ui, server_host(), Surface::Diagnostics);
        diagnosis_flow::apply_bundle_exported(
            &workspace,
            &workspace.diagnosis.borrow(),
            host,
            request,
            &report,
        );
        assert_eq!(
            first_view(&ui).bundle_result,
            "",
            "another host's export is not shown here"
        );
    }

    #[test]
    fn stopping_a_diagnosis_cancels_its_reported_operation() {
        let (ui, workspace, commands, host) = diagnostics_surface();
        ui.invoke_diagnose_host("client".into(), host.id().into());
        let _ = sent_request(&commands);
        let activity = ActivityData {
            item: crate::presentation::ActivityPresentation {
                id: "operation-7".to_owned(),
                title: lookup("operation-diagnosing-host"),
                status: String::new(),
                status_kind: "running",
                detail: "mods/big.jar".to_owned(),
                progress: 0.5,
                indeterminate: false,
                cancellable: true,
                failed: false,
            },
            entity: host.id(),
            time: String::new(),
        };
        diagnosis_flow::apply_activity(
            &ui,
            &workspace,
            &mut workspace.diagnosis.borrow_mut(),
            &activity,
        );
        assert_eq!(first_view(&ui).diagnosis_progress, "mods/big.jar");

        ui.invoke_stop_diagnosis();
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::CancelOperation(operation)) if operation == "operation-7"
        ));
    }

    #[test]
    fn a_pending_start_shows_the_stage_its_operation_reports() {
        let ui = test_window();
        let host = InstalledHost::Instance(ephervex_model::InstanceId::new());
        let mut flow = IsolationFlowState::default();
        flow.dispatch(host, IsolationMutation::Start);
        let activity = |entity: String, title: &str, detail: &str| ActivityData {
            item: crate::presentation::ActivityPresentation {
                id: "operation".to_owned(),
                title: lookup(title),
                status: String::new(),
                status_kind: "running",
                detail: detail.to_owned(),
                progress: 0.0,
                indeterminate: true,
                cancellable: true,
                failed: false,
            },
            entity,
            time: String::new(),
        };

        apply_isolation_stage(
            &ui,
            &flow,
            &activity(host.id(), "operation-canceling-isolation", "Elsewhere"),
        );
        apply_isolation_stage(
            &ui,
            &flow,
            &activity(
                "other".to_owned(),
                "operation-starting-isolation",
                "Elsewhere",
            ),
        );
        assert_eq!(ui.get_host_busy_text(), "");
        let stage = lookup("operation-stage-snapshot-isolation");
        apply_isolation_stage(
            &ui,
            &flow,
            &activity(host.id(), "operation-starting-isolation", &stage),
        );
        assert_eq!(ui.get_host_busy_text(), stage.as_str());
    }

    fn planned_setup(basis: u64) -> (AppWindow, Workspace, InstalledHost) {
        let (ui, workspace, _, host) = isolation_window();
        let mut setup = isolation_data(&host.id(), false, "setup");
        setup.basis = Some(basis);
        setup.candidates = candidates(&["a"]);
        apply_isolation_data(&workspace, setup);
        (ui, workspace, host)
    }

    #[test]
    fn a_mod_change_retires_a_setup_planned_before_it() {
        let (ui, workspace, host) = planned_setup(7);
        if let Some(setup) = workspace.isolation.borrow_mut().setup_mut(host) {
            setup.pinned.insert(PathBuf::from("mods/a.jar"));
        }

        apply_isolation_basis(&workspace, host, 7);
        assert_eq!(
            first_view(&ui).isolation_phase,
            "setup",
            "an unchanged host keeps its setup"
        );
        let other = client_host();
        apply_isolation_basis(&workspace, other, 8);
        assert_eq!(
            first_view(&ui).isolation_phase,
            "setup",
            "another host's change is not this one's"
        );

        apply_isolation_basis(&workspace, host, 8);
        assert_eq!(first_view(&ui).isolation_phase, "none");
        assert_eq!(first_view(&ui).isolation_candidates.row_count(), 0);
        let flow = workspace.isolation.borrow();
        let setup = flow.setup(host).expect("loaded");
        assert!(setup.pinned.is_empty());
        assert!(setup.setup_basis.is_none());
    }

    #[test]
    fn a_mod_change_never_discards_an_active_session_or_a_start_in_flight() {
        let (ui, workspace, host) = planned_setup(7);
        workspace
            .isolation
            .borrow_mut()
            .dispatch(host, IsolationMutation::Start)
            .expect("a start is sent");
        apply_isolation_basis(&workspace, host, 8);
        assert_eq!(first_view(&ui).isolation_phase, "setup");

        let request = workspace
            .isolation
            .borrow()
            .pending
            .map(|pending| pending.request);
        workspace
            .isolation
            .borrow_mut()
            .settle(request.unwrap_or_default());
        apply_isolation_data(&workspace, isolation_data(&host.id(), true, "round"));
        apply_isolation_basis(&workspace, host, 9);
        assert_eq!(first_view(&ui).isolation_phase, "round");
        assert!(first_view(&ui).isolation_active);
    }

    #[test]
    fn isolation_candidate_filtering_preserves_the_pinned_selection() {
        let mut setup = IsolationSetup {
            candidates: vec![
                backend::IsolationCandidateData {
                    path: "mods/alpha.jar".to_owned(),
                    display_path: "alpha.jar".to_owned(),
                    name: "Alpha".to_owned(),
                },
                backend::IsolationCandidateData {
                    path: "mods/beta.jar".to_owned(),
                    display_path: "beta.jar".to_owned(),
                    name: "Beta Library".to_owned(),
                },
            ],
            query: "beta".to_owned(),
            ..IsolationSetup::default()
        };
        setup.pinned.insert(PathBuf::from("mods/beta.jar"));

        let rows = isolation_candidate_rows(&setup);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Beta Library");
        assert!(
            rows[0].selected,
            "filtering must not clear a pinned candidate"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_row_action_on_a_jar_that_is_not_valid_utf8_addresses_that_exact_file() {
        use std::os::unix::ffi::OsStringExt as _;

        let odd = PathBuf::from(std::ffi::OsString::from_vec(b"mods/odd\xff.jar".to_vec()));
        let key = SharedString::from(mod_path_key::encode(&odd));
        let ui = test_window();
        let (sender, commands) = std::sync::mpsc::channel();
        register_mod_state_actions(
            &ui,
            &sender,
            &Rc::new(RefCell::new(LocalModDrop::default())),
        );
        let id = SharedString::from(ephervex_model::InstanceId::new().to_string());

        ui.invoke_set_mod_enabled("client".into(), id.clone(), key.clone(), false);
        ui.invoke_restore_mod_file("client".into(), id, key);

        let sent: Vec<_> = commands.try_iter().collect();
        assert!(
            matches!(
                sent.as_slice(),
                [
                    Command::SetModEnabled { path: toggled, enabled: false, .. },
                    Command::RestoreModFile { path: restored, .. },
                ] if *toggled == odd && *restored == odd
            ),
            "{sent:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pinned_jar_that_is_not_valid_utf8_stays_selected_in_the_candidate_list() {
        use std::os::unix::ffi::OsStringExt as _;

        let odd = PathBuf::from(std::ffi::OsString::from_vec(b"mods/odd\xff.jar".to_vec()));
        let mut setup = IsolationSetup {
            candidates: vec![backend::IsolationCandidateData {
                path: mod_path_key::encode(&odd),
                display_path: "odd".to_owned(),
                name: "Odd".to_owned(),
            }],
            ..IsolationSetup::default()
        };
        setup.pinned.insert(odd);

        let rows = isolation_candidate_rows(&setup);

        assert_eq!(rows.len(), 1);
        assert!(rows[0].selected);
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

    #[test]
    fn a_standalone_servers_suggested_name_never_exceeds_the_name_limit() {
        let ui = test_window();
        ui.set_create_server_client_id(SharedString::default());
        ui.set_create_server_minecraft("1.21.1".into());
        ui.set_create_server_loader("NeoForge".into());
        ui.set_create_server_loader_version("9".repeat(200).into());
        recompute_create_server_name(&ui);

        let suggestion = ui.get_create_server_name_suggestion();
        assert!(suggestion.ends_with(" Server"), "{suggestion}");
        ephervex_core::validated_entity_name(suggestion.as_str())
            .expect("a suggestion the user accepts is a name creation accepts");
    }

    #[test]
    fn a_linked_servers_suggested_name_is_one_creation_accepts_for_any_client_name() {
        let ui = test_window();
        register_name_rules(&ui);
        let rules = ui.global::<NameRules>();

        assert_eq!(
            rules.invoke_linked_server_name("Repro".into()),
            "Repro Server"
        );
        let long = "c".repeat(ephervex_core::MAX_ENTITY_NAME_CHARS);
        let suggestion = rules.invoke_linked_server_name(long.into());
        assert!(suggestion.ends_with(" Server"), "{suggestion}");
        ephervex_core::validated_entity_name(suggestion.as_str())
            .expect("accepting the suggestion must not fail creation");
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

    /// Each shown host keeps its own list and filter: a filter typed for one
    /// host never narrows another's, and a list for one never lands on the other.
    #[test]
    fn each_shown_host_keeps_its_own_mod_list_and_filter() {
        let ui = test_window();
        let workspace = test_workspace(&ui);
        let (client, server) = (client_host(), server_host());
        workspace.open(&ui, client, Surface::Mods);
        let first = workspace.single_pane().expect("one pane");
        workspace.change_pane(&ui, first, |layout| layout.split_beside(first).is_ok());
        ui.invoke_follow_host(2, "server".into(), server.id().into(), "Mods".into());

        let mut mods = workspace.mods.borrow_mut();
        assert!(mods.apply_list(
            client,
            vec![
                mod_row("Sodium", "sodium.jar", ""),
                mod_row("Iris", "iris.jar", "")
            ],
            String::new(),
            false,
        ));
        assert!(mods.apply_list(
            server,
            vec![mod_row("Lithium", "lithium.jar", "")],
            String::new(),
            false,
        ));
        assert!(mods.filter(client, "iris"));
        assert!(
            !mods.apply_list(client_host(), Vec::new(), String::new(), false),
            "a list for a host no pane shows lands nowhere"
        );
        drop(mods);
        for host in [client, server] {
            mod_views::publish(&workspace, &workspace.mods.borrow(), host);
        }

        let views = ui.get_host_views();
        let titles = |index: usize| -> Vec<String> {
            views
                .row_data(index)
                .expect("view")
                .mods
                .iter()
                .map(|row| row.title.to_string())
                .collect()
        };
        assert_eq!(titles(0), ["Iris"], "the client's own filter");
        assert_eq!(titles(1), ["Lithium"], "the server's list is unfiltered");
    }

    /// A file dropped on the window carries no cursor position, so what it is
    /// for is decided by what the window is asking for. Every state the user
    /// can be in when they let go has to have an answer.
    #[test]
    fn a_drop_belongs_to_whichever_dialog_is_asking_for_an_archive() {
        let ui = test_window();
        let closed = WorkspaceState::default();

        ui.set_modal(SharedString::default());
        assert_eq!(drop_intent(&ui, &closed), DropIntent::Import);

        ui.set_modal("create-server".into());
        assert_eq!(
            drop_intent(&ui, &closed),
            DropIntent::ServerPack(ServerPackTarget::NewServer)
        );

        ui.set_modal("import".into());
        ui.set_import_create_server(false);
        assert_eq!(
            drop_intent(&ui, &closed),
            DropIntent::Import,
            "an import with no server wanted is still asking for the pack"
        );

        ui.set_import_create_server(true);
        assert_eq!(
            drop_intent(&ui, &closed),
            DropIntent::ServerPack(ServerPackTarget::Import),
            "the pack is already chosen by the time that toggle can be reached"
        );

        ui.set_modal("delete-server".into());
        assert_eq!(
            drop_intent(&ui, &closed),
            DropIntent::Ignored,
            "a file let go over a confirmation is a misplaced gesture"
        );
    }

    /// A jar let go over a visible Mods list goes to that list's host, and
    /// with two hosts' lists on screen there is no telling which was meant, so
    /// the drop is ignored rather than opening a pack import.
    #[test]
    fn a_dropped_jar_goes_to_the_one_mods_list_on_screen() {
        let ui = test_window();
        let workspace = test_workspace(&ui);
        let (client, server) = (client_host(), server_host());
        ui.set_modal(SharedString::default());

        workspace.open(&ui, client, Surface::Mods);
        assert_eq!(
            drop_intent(&ui, &workspace.state.borrow()),
            DropIntent::Mods(client)
        );

        let first = workspace.single_pane().expect("one pane");
        workspace.change_pane(&ui, first, |layout| layout.split_beside(first).is_ok());
        let second = PaneId::from_raw(2);
        ui.invoke_show_pane_surface(2, "Mods".into());
        assert_eq!(
            drop_intent(&ui, &workspace.state.borrow()),
            DropIntent::Mods(client),
            "two panes on one host's list are still one list"
        );

        workspace.change_pane(&ui, second, |layout| {
            layout.show_host(second, server).is_ok()
        });
        assert_eq!(
            drop_intent(&ui, &workspace.state.borrow()),
            DropIntent::Ignored,
            "two hosts' lists leave the drop without a target"
        );

        ui.invoke_close_workspace();
        assert_eq!(
            drop_intent(&ui, &workspace.state.borrow()),
            DropIntent::Import
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

    fn neoforge_server_files() -> ServerArchiveEvidence {
        ServerArchiveEvidence {
            carries_mods: true,
            loader: Some(ephervex_model::LoaderKind::NeoForge),
            loader_version: Some("21.1.77".to_owned()),
            minecraft_version: Some("1.21.1".to_owned()),
        }
    }

    /// The selection a standalone server's files name is filled in, and the
    /// dialog says where it came from.
    #[test]
    fn server_files_that_name_their_selection_fill_in_a_standalone_server() {
        let ui = test_window();
        let archive = Path::new("/tmp/ServerFiles-4.2.zip");
        ui.set_create_server_client_id(SharedString::default());
        ui.set_create_server_minecraft("26.1.2".into());
        ui.set_create_server_loader("Vanilla".into());
        set_server_pack(&ui, ServerPackTarget::NewServer, Some(archive));

        apply_server_pack_evidence(&ui, archive, Ok(neoforge_server_files()));

        assert_eq!(ui.get_create_server_minecraft(), "1.21.1");
        assert_eq!(ui.get_create_server_loader(), "NeoForge");
        assert_eq!(ui.get_create_server_loader_version(), "21.1.77");
        assert_eq!(
            ui.get_create_server_pack_note().as_str(),
            lookup_args(
                "gui-server-pack-selection-read",
                [
                    ("minecraft", Arg::from("1.21.1")),
                    ("loader", Arg::from("NeoForge")),
                    ("version", Arg::from("21.1.77")),
                ],
            )
        );

        apply_loader_versions(
            &ui,
            "server",
            ephervex_model::LoaderKind::NeoForge,
            "1.21.1",
            &["21.1.90".to_owned(), "21.1.77".to_owned()],
        );
        assert_eq!(
            ui.get_create_server_loader_version(),
            "21.1.77",
            "a listing arriving later does not replace the build the files name"
        );
    }

    /// Mods with no readable loader leave the selection to the user and say so,
    /// and a linked server's selection is never touched.
    #[test]
    fn server_files_that_do_not_name_a_loader_leave_the_selection_alone() {
        let ui = test_window();
        let archive = Path::new("/tmp/ServerFiles-4.2.zip");
        ui.set_create_server_client_id(SharedString::default());
        ui.set_create_server_minecraft("26.1.2".into());
        ui.set_create_server_loader("Vanilla".into());
        set_server_pack(&ui, ServerPackTarget::NewServer, Some(archive));

        apply_server_pack_evidence(
            &ui,
            archive,
            Ok(ServerArchiveEvidence {
                carries_mods: true,
                ..ServerArchiveEvidence::default()
            }),
        );
        assert_eq!(ui.get_create_server_minecraft(), "26.1.2");
        assert_eq!(ui.get_create_server_loader(), "Vanilla");
        assert!(ui.get_create_server_pack_mods());
        assert_eq!(
            ui.get_create_server_pack_note().as_str(),
            lookup("gui-server-pack-selection-unknown")
        );

        ui.set_create_server_client_id("repro-client".into());
        set_server_pack(&ui, ServerPackTarget::NewServer, Some(archive));
        apply_server_pack_evidence(&ui, archive, Ok(neoforge_server_files()));
        assert_eq!(ui.get_create_server_loader(), "Vanilla");
        assert_eq!(ui.get_create_server_pack_note(), "");
    }

    /// An answer about files the dialog no longer shows changes nothing.
    #[test]
    fn evidence_for_replaced_server_files_is_dropped() {
        let ui = test_window();
        ui.set_create_server_client_id(SharedString::default());
        ui.set_create_server_loader("Vanilla".into());
        set_server_pack(
            &ui,
            ServerPackTarget::NewServer,
            Some(Path::new("/tmp/second.zip")),
        );

        apply_server_pack_evidence(
            &ui,
            Path::new("/tmp/first.zip"),
            Ok(neoforge_server_files()),
        );

        assert_eq!(ui.get_create_server_loader(), "Vanilla");
        assert!(!ui.get_create_server_pack_mods());
    }

    #[test]
    fn the_dialogs_conflict_rule_is_the_one_creation_refuses_with() {
        let files = neoforge_server_files();
        let vanilla = ServerPackSelection {
            minecraft: "1.21.1",
            loader: "Vanilla",
            loader_version: "",
        };
        assert_eq!(
            server_pack_conflict(&vanilla, &files),
            lookup_args(
                "error-server-archive-loader-differs",
                [
                    ("archive", Arg::from("NeoForge")),
                    ("selected", Arg::from("Vanilla")),
                ],
            )
        );
        let matching = ServerPackSelection {
            minecraft: "1.21.1",
            loader: "NeoForge",
            loader_version: "21.1.77",
        };
        assert_eq!(server_pack_conflict(&matching, &files), "");
        let unchosen_build = ServerPackSelection {
            loader_version: "",
            ..matching
        };
        assert_eq!(
            server_pack_conflict(&unchosen_build, &files),
            "",
            "the dialog already asks for a build"
        );
        let mods_only = ServerArchiveEvidence {
            carries_mods: true,
            ..ServerArchiveEvidence::default()
        };
        assert!(has("error-server-archive-needs-loader"));
        assert_ne!(server_pack_conflict(&vanilla, &mods_only), "");
    }

    fn test_workspace(ui: &AppWindow) -> Workspace {
        test_workspace_sending(ui).0
    }

    /// A start plans its Java before it can fail, and may have installed a
    /// managed runtime by then, so even an abandoned start re-reads what the
    /// shown hosts run.
    #[test]
    fn an_abandoned_server_start_reads_the_shown_hosts_java_again() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        let host = client_host();
        workspace.open(&ui, host, Surface::Diagnostics);
        commands.try_iter().for_each(drop);

        apply_server_tool_event(
            &ui,
            &workspace,
            ephervex_model::ServerId::new(),
            backend::ServerToolEvent::StartAbandoned,
        );

        assert!(commands.try_iter().any(|command| matches!(
            command,
            Command::LoadHostJava { host: loaded, choices: true } if loaded == host
        )));
    }

    /// The launcher preference can pin a listed runtime, so a saved Java
    /// setting reads the list, and its pins, again.
    #[test]
    fn a_saved_java_setting_reads_the_runtime_list_again() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        apply_event(
            &ui,
            Event::SettingsSaveFinished {
                save: SettingsSave::JavaSettings,
                succeeded: true,
            },
            &Rc::default(),
            &Rc::default(),
            &Rc::default(),
            &Rc::default(),
            &Rc::default(),
            &Rc::default(),
            &Rc::default(),
            &Rc::default(),
            &Rc::default(),
            &workspace,
            &workspace.commands,
        );

        assert!(
            commands
                .try_iter()
                .any(|command| matches!(command, Command::LoadJavaRuntimes))
        );
    }

    /// A workspace whose commands can be read back.
    fn test_workspace_sending(ui: &AppWindow) -> (Workspace, std::sync::mpsc::Receiver<Command>) {
        let (sender, commands) = std::sync::mpsc::channel();
        let workspace = Workspace {
            state: Rc::default(),
            lifecycle: Rc::default(),
            logs: Rc::new(RefCell::new(LogViews::new(sender.clone()))),
            mods: Rc::default(),
            browser: Rc::default(),
            isolation: Rc::default(),
            diagnosis: Rc::default(),
            acquisition_prompts: Rc::default(),
            stand_in_prompts: Rc::default(),
            plan_requests: Rc::default(),
            properties: Rc::default(),
            commands: sender,
        };
        register_workspace(ui, &workspace);
        log_views::register(ui, &workspace.logs);
        (workspace, commands)
    }

    #[test]
    fn disconnected_command_worker_refuses_submission_and_releases_the_server_guard() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_server_actions(&ui, &workspace.commands, &workspace);
        let server = ephervex_model::ServerId::new();
        let row = ServerRow {
            id: server.to_string().into(),
            state_kind: "running".into(),
            can_send_command: true,
            ..ServerRow::default()
        };
        workspace
            .lifecycle
            .borrow_mut()
            .servers
            .observe(std::slice::from_ref(&row));
        workspace.state.borrow_mut().set_rows(Vec::new(), vec![row]);
        workspace.open(&ui, InstalledHost::Server(server), Surface::Console);
        drop(commands);
        assert!(!ui.invoke_server_console(server.to_string().into(), "list".into()));
        let view = ui.get_host_views().row_data(0).expect("server view");
        assert!(view.server.can_send_command);
        assert!(!view.server.console_delivered);
        assert!(!workspace.lifecycle.borrow().servers.busy(server));
    }

    #[test]
    fn repeated_server_panes_share_command_delivery_and_target_the_displayed_host() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_server_actions(&ui, &workspace.commands, &workspace);
        let server = ephervex_model::ServerId::new();
        let other = ephervex_model::ServerId::new();
        let rows = [server, other].map(|id| ServerRow {
            id: id.to_string().into(),
            state_kind: "running".into(),
            can_stop: true,
            can_restart: true,
            can_send_command: true,
            ..ServerRow::default()
        });
        workspace.lifecycle.borrow_mut().servers.observe(&rows);
        workspace
            .state
            .borrow_mut()
            .set_rows(Vec::new(), rows.to_vec());
        workspace.open(&ui, InstalledHost::Server(server), Surface::Console);
        ui.invoke_split_pane_beside(1);
        ui.invoke_show_pane_surface(2, "Console".into());
        assert_eq!(ui.get_host_views().row_count(), 1);
        assert_eq!(ui.get_workspace_panes().row_count(), 2);
        commands.try_iter().for_each(drop);
        assert!(!ui.invoke_server_console(server.to_string().into(), "   ".into()));
        assert!(ui.invoke_server_console(server.to_string().into(), "list".into()));
        assert!(!ui.invoke_server_console(server.to_string().into(), "say duplicate".into()));
        let sent: Vec<_> = commands.try_iter().collect();
        assert!(
            matches!(sent.as_slice(), [Command::SendConsole { server: target, command }] if *target == server && command == "list")
        );
        assert!(
            !ui.get_host_views()
                .row_data(0)
                .expect("server view")
                .server
                .can_send_command
        );
        workspace.lifecycle.borrow_mut().servers.apply(
            server,
            backend::ServerToolEvent::ConsoleFinished {
                outcome: backend::ConsoleOutcome::Delivered,
            },
        );
        workspace.refresh(&ui);
        assert!(
            ui.get_host_views()
                .row_data(0)
                .expect("server view")
                .server
                .can_send_command
        );
        commands.try_iter().for_each(drop);
        ui.invoke_open_server_location(other.to_string().into(), "config".into());
        ui.invoke_open_server_location(server.to_string().into(), "capture".into());
        ui.invoke_open_server_location(server.to_string().into(), "../outside".into());
        let sent: Vec<_> = commands.try_iter().collect();
        assert!(
            matches!(sent.as_slice(), [Command::OpenServerLocation { server: target, folder: ephervex_core::ServerFolder::Config }, Command::OpenServerCapture(captured)] if *target == other && *captured == server)
        );
    }

    #[test]
    fn intentional_server_controls_do_not_request_failure_diagnostics_in_either_order() {
        for action in [ServerAction::Stop, ServerAction::Kill] {
            for exit_first in [false, true] {
                let ui = test_window();
                let (workspace, commands, client, host) = paired_workspace(&ui);
                let InstalledHost::Server(server) = host else {
                    unreachable!()
                };
                let (clients, rows) = paired_rows(client, host, false, true);
                workspace.lifecycle.borrow_mut().servers.observe(&rows);
                workspace.state.borrow_mut().set_rows(clients, rows);
                workspace.open(&ui, host, Surface::Console);
                ui.invoke_split_pane_beside(1);
                commands.try_iter().for_each(drop);
                workspace
                    .lifecycle
                    .borrow_mut()
                    .servers
                    .apply(server, backend::ServerToolEvent::Started);
                assert!(
                    workspace
                        .lifecycle
                        .borrow_mut()
                        .servers
                        .request(server, action, false)
                );
                let exit = backend::ServerToolEvent::Finished {
                    failure: Some("nonzero exit".to_owned()),
                };
                let control = backend::ServerToolEvent::StopFinished { succeeded: true };
                for event in if exit_first {
                    [exit, control]
                } else {
                    [control, exit]
                } {
                    apply_server_tool_event(&ui, &workspace, server, event);
                }
                assert!(
                    commands
                        .try_iter()
                        .all(|event| !matches!(event, Command::ExplainServerFailure { .. }))
                );
                assert!(run_notices(&ui).iter().all(String::is_empty));
            }
        }
    }

    #[test]
    fn a_genuine_server_failure_requests_one_diagnosis_without_navigation() {
        let ui = test_window();
        let (workspace, commands, _, host) = paired_workspace(&ui);
        let InstalledHost::Server(server) = host else {
            unreachable!()
        };
        workspace.open(&ui, host, Surface::Console);
        commands.try_iter().for_each(drop);
        apply_server_tool_event(
            &ui,
            &workspace,
            server,
            backend::ServerToolEvent::RestartFailed {
                failure: "replacement failed".to_owned(),
            },
        );
        apply_server_tool_event(
            &ui,
            &workspace,
            server,
            backend::ServerToolEvent::Finished {
                failure: Some("replacement failed".to_owned()),
            },
        );
        // The shown hosts also read their Java again; only diagnoses count here.
        let sent: Vec<_> = commands
            .try_iter()
            .filter(|command| !matches!(command, Command::LoadHostJava { .. }))
            .collect();
        assert!(
            matches!(sent.as_slice(), [Command::ExplainServerFailure { server: id, failure, .. }]
            if *id == server && failure == "replacement failed")
        );
        fail_run(&workspace, host);
        assert!(!run_notices(&ui)[0].is_empty());
        assert_eq!(shown(&workspace), [(host, Surface::Console)]);
        apply_server_tool_event(&ui, &workspace, server, backend::ServerToolEvent::Started);
        assert!(run_notices(&ui).iter().all(String::is_empty));
    }

    #[test]
    fn a_start_refused_for_missing_eula_consent_offers_the_eula_review_instead_of_a_failed_run() {
        let ui = test_window();
        let (workspace, commands, _, host) = paired_workspace(&ui);
        diagnosis_flow::register(&ui, &workspace.commands, &workspace);
        let InstalledHost::Server(server) = host else {
            unreachable!()
        };
        workspace.open(&ui, host, Surface::Console);
        ui.invoke_server_start(server.to_string().into());
        assert_eq!(view_of(&ui, host).server.state_kind, "starting");
        commands.try_iter().for_each(drop);
        let message = backend::localized_error(&ephervex_core::CoreError::from(
            ephervex_server::ServerError::EulaNotAccepted,
        ));
        apply_server_tool_event(
            &ui,
            &workspace,
            server,
            backend::ServerToolEvent::EulaRefused {
                message: message.clone(),
            },
        );

        assert!(
            commands
                .try_iter()
                .all(|command| !matches!(command, Command::ExplainServerFailure { .. })),
            "nothing ran, so nothing is diagnosed"
        );
        let view = view_of(&ui, host);
        assert_eq!(view.server.state_kind, "stopped");
        assert!(view.server.can_start);
        assert_eq!(view.run_failure_title, lookup("gui-run-refused-server"));
        assert!(view.run_eula_refused);
        assert!(view.run_likely_cause.is_empty());
        assert!(view.run_crash_report.is_empty());
        assert_eq!(view.run_failure, message);
        assert_eq!(
            message,
            format!(
                "{}\n{}",
                lookup("error-server-eula-not-accepted"),
                ephervex_i18n::lookup_attribute("error-server-eula-not-accepted", "desktop-action")
                    .expect("a desktop next step")
            ),
            "the desktop app names its own way to accept, not the CLI command"
        );
        assert_eq!(
            view.server.console_status,
            lookup("gui-server-console-stopped")
        );

        ui.invoke_dismiss_run_failure("server".into(), server.to_string().into());
        assert!(run_notices(&ui).iter().all(String::is_empty));
    }

    #[test]
    fn linked_starts_claim_the_server_once_and_running_servers_remain_reusable() {
        for from_client in [false, true] {
            let ui = test_window();
            let (workspace, commands, client, host) = paired_workspace_starting_together(&ui, true);
            let InstalledHost::Server(server) = host else {
                unreachable!()
            };
            if from_client {
                ui.invoke_launch_client(client.id().into());
            } else {
                ui.invoke_server_start(host.id().into());
            }
            assert!(workspace.lifecycle.borrow().servers.busy(server));
            ui.invoke_server_start(host.id().into());
            let sent = starts(&commands);
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0].0, "pair");
            apply_server_tool_event(&ui, &workspace, server, backend::ServerToolEvent::Started);
            let (clients, rows) = paired_rows(client, host, true, true);
            workspace.lifecycle.borrow_mut().servers.observe(&rows);
            workspace.state.borrow_mut().set_rows(clients, rows);
            apply_pair_start_settled(&ui, &workspace, &workspace.lifecycle, sent[0].2, true);
            assert!(!workspace.lifecycle.borrow().servers.busy(server));
            assert!(!workspace.lifecycle.borrow().pair_starting(host));
            assert!(
                workspace
                    .lifecycle
                    .borrow_mut()
                    .servers
                    .claim_linked(server, false)
                    .is_ok()
            );
            apply_server_tool_event(
                &ui,
                &workspace,
                server,
                backend::ServerToolEvent::Finished { failure: None },
            );
            let (_, rows) = paired_rows(client, host, true, false);
            workspace.lifecycle.borrow_mut().servers.observe(&rows);
            assert!(workspace.lifecycle.borrow_mut().servers.request(
                server,
                ServerAction::Start,
                false
            ));
        }
    }

    fn client_host() -> InstalledHost {
        InstalledHost::Instance(ephervex_model::InstanceId::new())
    }

    fn server_host() -> InstalledHost {
        InstalledHost::Server(ephervex_model::ServerId::new())
    }

    fn server_host_id(host: InstalledHost) -> ephervex_model::ServerId {
        match host {
            InstalledHost::Server(id) => id,
            InstalledHost::Instance(_) => panic!("not a server host"),
        }
    }

    /// Every pane's host and surface, in reading order.
    fn shown(workspace: &Workspace) -> Vec<(InstalledHost, Surface)> {
        workspace
            .state
            .borrow()
            .layout()
            .map(|layout| {
                layout
                    .panes()
                    .iter()
                    .map(|placed| (placed.pane.host(), placed.pane.surface()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A workspace listing one client and one server, as a snapshot fills it.
    fn listed_workspace(ui: &AppWindow) -> (Workspace, InstalledHost, InstalledHost) {
        let workspace = test_workspace(ui);
        let (client, server) = (client_host(), server_host());
        workspace.state.borrow_mut().set_rows(
            vec![InstanceRow {
                id: client.id().into(),
                name: "Repro Client".into(),
                ..InstanceRow::default()
            }],
            vec![ServerRow {
                id: server.id().into(),
                name: "Repro Server".into(),
                can_restart: true,
                can_start: true,
                ..ServerRow::default()
            }],
        );
        workspace
            .lifecycle
            .borrow_mut()
            .servers
            .observe(&[ServerRow {
                id: server.id().into(),
                can_start: true,
                ..ServerRow::default()
            }]);
        (workspace, client, server)
    }

    type PairedWorkspace = (
        Workspace,
        std::sync::mpsc::Receiver<Command>,
        InstalledHost,
        InstalledHost,
    );

    /// A workspace listing a client paired with its linked server.
    fn paired_workspace(ui: &AppWindow) -> PairedWorkspace {
        paired_workspace_starting_together(ui, false)
    }

    fn paired_rows(
        client: InstalledHost,
        server: InstalledHost,
        start_together: bool,
        running: bool,
    ) -> (Vec<InstanceRow>, Vec<ServerRow>) {
        (
            vec![InstanceRow {
                id: client.id().into(),
                name: "Repro Client".into(),
                paired_server_id: server.id().into(),
                paired_server_name: "Repro Server".into(),
                pair_start_together: start_together,
                can_launch: !running,
                can_kill: running,
                ..InstanceRow::default()
            }],
            vec![ServerRow {
                id: server.id().into(),
                name: "Repro Server".into(),
                linked_client_id: client.id().into(),
                paired_client_id: client.id().into(),
                paired_client_name: "Repro Client".into(),
                pair_start_together: start_together,
                can_start: !running,
                can_stop: running,
                can_kill: running,
                can_restart: running,
                can_send_command: running,
                state_kind: if running { "running" } else { "stopped" }.into(),
                ..ServerRow::default()
            }],
        )
    }

    fn paired_workspace_starting_together(ui: &AppWindow, start_together: bool) -> PairedWorkspace {
        let (workspace, commands) = test_workspace_sending(ui);
        register_host_starts(ui, &workspace);
        let (client, server) = (client_host(), server_host());
        let (clients, servers) = paired_rows(client, server, start_together, false);
        workspace.lifecycle.borrow_mut().servers.observe(&servers);
        workspace.state.borrow_mut().set_rows(clients, servers);
        (workspace, commands, client, server)
    }

    /// The starts sent, as (what, initiator).
    fn starts(
        commands: &std::sync::mpsc::Receiver<Command>,
    ) -> Vec<(&'static str, InstalledHost, u64)> {
        commands
            .try_iter()
            .filter_map(|command| match command {
                Command::LaunchClient { instance, .. } => {
                    Some(("launch", InstalledHost::Instance(instance), 0))
                }
                Command::StartServer(server) => Some(("start", InstalledHost::Server(server), 0)),
                Command::LaunchAndConnect { client, .. } => {
                    Some(("connect", InstalledHost::Instance(client), 0))
                }
                Command::StartLinked {
                    initiator,
                    operation,
                } => Some(("pair", initiator, operation)),
                Command::RestartHost { host, request } => Some(("restart", host, request)),
                _ => None,
            })
            .collect()
    }

    fn fail_run(workspace: &Workspace, host: InstalledHost) {
        diagnosis_flow::apply_run_failed(
            workspace,
            &mut workspace.diagnosis.borrow_mut(),
            host,
            lookup("gui-run-exited-with-error"),
            None,
            None,
        );
    }

    fn run_notices(ui: &AppWindow) -> Vec<String> {
        ui.get_host_views()
            .iter()
            .map(|view| view.run_failure_title.to_string())
            .collect()
    }

    #[test]
    fn starting_a_host_again_forgets_its_failed_run() {
        let ui = test_window();
        let (workspace, _commands, client, _server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Overview);
        fail_run(&workspace, client);
        assert_eq!(run_notices(&ui), [lookup("gui-run-failed-client")]);

        ui.invoke_launch_client(client.id().into());
        assert_eq!(run_notices(&ui), [String::new()]);
    }

    #[test]
    fn starting_a_host_again_forgets_its_crash_banner() {
        let crash_reports = |ui: &AppWindow| -> Vec<String> {
            ui.get_host_views()
                .iter()
                .map(|view| view.crash_report.to_string())
                .collect()
        };
        let crashed = |workspace: &Workspace, host: InstalledHost| {
            workspace.update(host, |view| {
                view.crash_report = "crash-2026-09-28_18.13.39-fml.txt".into();
                view.crash_failure = "MISSING EXCEPTION MESSAGE".into();
                view.crash_suggested = 2;
            });
        };
        let ui = test_window();
        let (workspace, _commands, _client, server) = paired_workspace(&ui);
        let InstalledHost::Server(id) = server else {
            unreachable!()
        };
        workspace.open(&ui, server, Surface::Overview);
        crashed(&workspace, server);
        assert_eq!(crash_reports(&ui), ["crash-2026-09-28_18.13.39-fml.txt"]);

        ui.invoke_server_start(id.to_string().into());
        assert_eq!(crash_reports(&ui), [String::new()]);
        assert!(
            ui.get_host_views()
                .iter()
                .all(|view| view.crash_suggested == 0 && view.crash_failure.is_empty())
        );

        // A start the user did not press here, such as a restart's new child.
        crashed(&workspace, server);
        apply_server_tool_event(&ui, &workspace, id, backend::ServerToolEvent::Started);
        assert_eq!(crash_reports(&ui), [String::new()]);
    }

    #[test]
    fn a_pair_start_forgets_both_sides_failed_runs() {
        let ui = test_window();
        let (workspace, _commands, client, server) = paired_workspace_starting_together(&ui, true);
        workspace.open(&ui, client, Surface::Overview);
        ui.invoke_split_pane_beside(1);
        ui.invoke_show_linked_pair(1, "server".into(), server.id().into());
        fail_run(&workspace, client);
        fail_run(&workspace, server);
        assert!(run_notices(&ui).iter().all(|notice| !notice.is_empty()));

        ui.invoke_launch_client(client.id().into());
        assert!(run_notices(&ui).iter().all(String::is_empty));
    }

    /// A failure reported by a client run that ended at `ended`.
    fn fail_client_run(workspace: &Workspace, client: InstalledHost, ended: u64) {
        apply_run_failure(
            workspace,
            &workspace.lifecycle,
            client,
            lookup("gui-run-exited-with-error"),
            None,
            None,
            backend::RunRevision::Client(ended),
        );
    }

    #[test]
    fn launch_and_connect_forgets_the_clients_failed_run() {
        let ui = test_window();
        let (workspace, _commands, client, server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Overview);
        fail_run(&workspace, client);
        assert_eq!(run_notices(&ui), [lookup("gui-run-failed-client")]);

        ui.invoke_launch_and_connect(client.id().into(), server.id().into());
        assert!(run_notices(&ui).iter().all(String::is_empty));
    }

    #[test]
    fn launching_as_an_account_or_alongside_forgets_the_clients_failed_run() {
        for alongside in [false, true] {
            let ui = test_window();
            let (workspace, commands, client, _server) = paired_workspace(&ui);
            workspace.open(&ui, client, Surface::Overview);
            fail_run(&workspace, client);
            assert_eq!(run_notices(&ui), [lookup("gui-run-failed-client")]);

            ui.invoke_launch_client_as(client.id().into(), "".into(), alongside);
            assert_eq!(run_notices(&ui), [String::new()], "alongside: {alongside}");
            assert_eq!(starts(&commands), [("launch", client, 0)]);
        }
    }

    #[test]
    fn a_failure_of_a_run_the_client_has_since_started_over_is_dropped() {
        let ui = test_window();
        let (workspace, _commands, client, _server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Overview);
        // A run that ended before the user launched again.
        let earlier = backend::run_clock().saturating_sub(1);
        ui.invoke_launch_client(client.id().into());

        fail_client_run(&workspace, client, earlier);
        assert_eq!(run_notices(&ui), [String::new()]);

        fail_client_run(&workspace, client, backend::run_clock());
        assert_eq!(run_notices(&ui), [lookup("gui-run-failed-client")]);
    }

    #[test]
    fn a_client_restart_starting_forgets_the_clients_failed_run() {
        let ui = test_window();
        let (workspace, _commands, client, _server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Overview);
        fail_run(&workspace, client);
        assert_eq!(run_notices(&ui), [lookup("gui-run-failed-client")]);
        let lifecycle = Rc::clone(&workspace.lifecycle);

        apply_host_restart(
            &ui,
            &workspace,
            &lifecycle,
            client,
            1,
            Some(backend::RestartStage::Stopping),
        );
        assert_eq!(run_notices(&ui), [lookup("gui-run-failed-client")]);

        workspace.forget_last_run_from(client, backend::run_clock());
        assert_eq!(run_notices(&ui), [String::new()]);
    }

    #[test]
    fn a_client_restart_drops_the_failure_of_a_run_that_ended_before_it_started() {
        let ui = test_window();
        let (workspace, _commands, client, _server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Overview);

        workspace.forget_last_run_from(client, 10);
        fail_client_run(&workspace, client, 9);
        assert_eq!(run_notices(&ui), [String::new()]);
    }

    #[test]
    fn a_failure_of_a_run_that_ended_before_a_client_restart_is_applied_still_shows() {
        let ui = test_window();
        let (workspace, _commands, client, _server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Overview);

        // The new run failed after the backend noted the clock, but before the
        // window applied that start.
        workspace.forget_last_run_from(client, 10);
        fail_client_run(&workspace, client, 10);
        assert_eq!(run_notices(&ui), [lookup("gui-run-failed-client")]);
    }

    #[test]
    fn a_client_restart_that_ends_without_starting_keeps_the_failed_run() {
        let ui = test_window();
        let (workspace, _commands, client, _server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Overview);
        fail_run(&workspace, client);
        let lifecycle = Rc::clone(&workspace.lifecycle);

        apply_host_restart(&ui, &workspace, &lifecycle, client, 1, None);
        assert_eq!(run_notices(&ui), [lookup("gui-run-failed-client")]);
    }

    #[test]
    fn dismissing_a_notice_does_not_drop_the_failure_of_a_run_that_ended_before() {
        let ui = test_window();
        let (workspace, _commands, client, _server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Overview);
        fail_run(&workspace, client);
        // A run that ended before the dismissal.
        let ended = backend::run_clock().saturating_sub(1);

        ui.invoke_dismiss_run_failure("client".into(), client.id().into());
        fail_client_run(&workspace, client, ended);
        assert_eq!(run_notices(&ui), [lookup("gui-run-failed-client")]);
    }

    #[test]
    fn a_run_still_alive_when_the_client_launches_alongside_shows_its_crash() {
        let ui = test_window();
        let (workspace, _commands, client, _server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Overview);
        ui.invoke_launch_client_as(client.id().into(), "".into(), true);

        // The earlier run ends after the second launch was requested.
        fail_client_run(&workspace, client, backend::run_clock());
        assert_eq!(run_notices(&ui), [lookup("gui-run-failed-client")]);
    }

    #[test]
    fn a_failure_of_a_client_run_with_no_later_start_is_shown() {
        let ui = test_window();
        let (workspace, _commands, client, _server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Overview);

        fail_client_run(&workspace, client, 0);
        assert_eq!(run_notices(&ui), [lookup("gui-run-failed-client")]);
    }

    #[test]
    fn a_pair_that_does_not_start_together_starts_only_the_side_asked_for() {
        let ui = test_window();
        let (_workspace, commands, client, server) = paired_workspace(&ui);
        ui.invoke_launch_client(client.id().into());
        ui.invoke_server_start(server.id().into());
        assert_eq!(
            starts(&commands),
            [("launch", client, 0), ("start", server, 0)]
        );
    }

    #[test]
    fn a_standalone_server_start_claims_shared_state_before_sending() {
        let ui = test_window();
        let (workspace, commands, _, server) = paired_workspace(&ui);
        workspace.open(&ui, server, Surface::Console);
        ui.invoke_split_pane_beside(1);
        ui.invoke_server_start(server.id().into());
        ui.invoke_server_start(server.id().into());
        assert_eq!(starts(&commands), [("start", server, 0)]);
        let InstalledHost::Server(id) = server else {
            unreachable!();
        };
        assert!(workspace.lifecycle.borrow().servers.busy(id));
        assert!(!workspace.lifecycle.borrow().pair_starting(server));
    }

    /// Launching the client is one pair start. The server coming up and the
    /// client running are not new triggers, so nothing else is sent.
    #[test]
    fn a_paired_launch_is_one_pair_start_and_nothing_starts_after_it() {
        let ui = test_window();
        let (workspace, commands, client, server) = paired_workspace_starting_together(&ui, true);
        workspace.open(&ui, client, Surface::Overview);
        ui.invoke_split_pane_beside(1);
        ui.invoke_show_linked_pair(1, "server".into(), server.id().into());

        ui.invoke_launch_client(client.id().into());
        let sent = starts(&commands);
        let [("pair", initiator, operation)] = sent.as_slice() else {
            panic!("one pair start, not {sent:?}");
        };
        assert_eq!(*initiator, client);
        let starting = |ui: &AppWindow| -> Vec<bool> {
            ui.get_host_views()
                .iter()
                .map(|view| {
                    if view.kind == "client" {
                        view.client.pair_starting
                    } else {
                        view.server.pair_starting
                    }
                })
                .collect()
        };
        assert_eq!(starting(&ui), [true, true], "both sides, in every pane");

        let (clients, servers) = paired_rows(client, server, true, true);
        workspace.state.borrow_mut().set_rows(clients, servers);
        workspace.refresh(&ui);
        apply_pair_start_settled(&ui, &workspace, &workspace.lifecycle, *operation, false);

        assert_eq!(starting(&ui), [false, false]);
        assert!(
            starts(&commands).is_empty(),
            "nothing follows the pair start"
        );
    }

    #[test]
    fn a_pair_start_from_either_side_puts_both_sides_on_screen() {
        let ui = test_window();
        let (workspace, _, client, server) = paired_workspace_starting_together(&ui, true);
        workspace.open(&ui, client, Surface::Mods);
        ui.invoke_launch_client(client.id().into());
        assert_eq!(
            shown(&workspace),
            [(client, Surface::Mods), (server, Surface::Console)]
        );

        let ui = test_window();
        let (workspace, _, client, server) = paired_workspace_starting_together(&ui, true);
        workspace.open(&ui, client, Surface::Overview);
        ui.invoke_server_start(server.id().into());
        assert_eq!(
            shown(&workspace),
            [(client, Surface::Overview), (server, Surface::Console)],
            "the client's pane keeps showing the client"
        );
    }

    /// Neither side can start again while its pair is starting, so there is
    /// no path from one side's start back to the other's.
    #[test]
    fn a_start_of_either_side_waits_while_its_pair_is_starting() {
        let ui = test_window();
        let toasts = toasts(&ui);
        let (_workspace, commands, client, server) = paired_workspace_starting_together(&ui, true);

        ui.invoke_server_start(server.id().into());
        ui.invoke_launch_client(client.id().into());
        ui.invoke_server_start(server.id().into());

        let sent = starts(&commands);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!((sent[0].0, sent[0].1), ("pair", server));
        assert_eq!(
            toasts.borrow().as_slice(),
            [
                (lookup("gui-pair-start-busy"), true),
                (lookup("gui-pair-start-busy"), true)
            ]
        );
    }

    /// A start that cannot claim the server says why, and leaves the pair free
    /// to start once the server can.
    #[test]
    fn a_start_together_refused_for_the_server_says_why_and_frees_the_pair() {
        let ui = test_window();
        let toasts = toasts(&ui);
        let (workspace, commands, client, server) = paired_workspace_starting_together(&ui, true);
        let server_id = server_host_id(server);

        // A console command to the running server is still in flight.
        let (clients, rows) = paired_rows(client, server, true, true);
        workspace.lifecycle.borrow_mut().servers.observe(&rows);
        workspace.state.borrow_mut().set_rows(clients, rows);
        assert!(workspace.lifecycle.borrow_mut().servers.request(
            server_id,
            ServerAction::Console,
            false
        ));
        ui.invoke_launch_client(client.id().into());
        ui.invoke_server_start(server.id().into());
        let busy = LinkedRefusal::Busy {
            server: "Repro Server".into(),
        };
        assert_eq!(
            busy.message(),
            lookup_args(
                "gui-pair-start-server-busy",
                [("server", Arg::from("Repro Server"))],
            )
        );
        assert_ne!(busy.message(), lookup("gui-pair-start-busy"));

        // A server that can neither start nor stop needs the user to act.
        let (clients, mut rows) = paired_rows(client, server, true, false);
        rows[0].installed = true;
        rows[0].can_start = false;
        rows[0].state = "Needs recovery".into();
        workspace.lifecycle.borrow_mut().servers = ServerTools::default();
        workspace.lifecycle.borrow_mut().servers.observe(&rows);
        workspace.state.borrow_mut().set_rows(clients, rows);
        ui.invoke_launch_client(client.id().into());
        ui.invoke_server_start(server.id().into());
        let unavailable = LinkedRefusal::Unavailable {
            server: "Repro Server".into(),
            state: "Needs recovery".into(),
        };

        assert_eq!(
            toasts.borrow().as_slice(),
            [
                (busy.message(), true),
                (busy.message(), true),
                (unavailable.message(), true),
                (unavailable.message(), true),
            ]
        );
        assert!(unavailable.message().contains("Needs recovery"));
        assert!(starts(&commands).is_empty(), "nothing launches");
        assert!(!workspace.lifecycle.borrow().pair_starting(client));
    }

    #[test]
    fn a_linked_claim_refuses_a_busy_restarting_or_unstartable_server_with_its_own_reason() {
        let client = client_host();
        let server = server_host();
        let server_id = server_host_id(server);
        let name = || "Repro Server".to_string();

        let mut stopping = ServerTools::default();
        let (_, rows) = paired_rows(client, server, true, true);
        stopping.observe(&rows);
        assert!(stopping.request(server_id, ServerAction::Stop, false));
        assert_eq!(
            stopping.claim_linked(server_id, false),
            Err(LinkedRefusal::Busy { server: name() })
        );

        let mut restarting = ServerTools::default();
        restarting.observe(&rows);
        assert_eq!(
            restarting.claim_linked(server_id, true),
            Err(LinkedRefusal::Restarting { server: name() })
        );

        let mut recovery = ServerTools::default();
        let (_, mut rows) = paired_rows(client, server, true, false);
        rows[0].installed = true;
        rows[0].can_start = false;
        rows[0].state = "Needs recovery".into();
        recovery.observe(&rows);
        assert_eq!(
            recovery.claim_linked(server_id, false),
            Err(LinkedRefusal::Unavailable {
                server: name(),
                state: "Needs recovery".into()
            })
        );

        let mut uninstalled = ServerTools::default();
        let (_, mut rows) = paired_rows(client, server, true, false);
        rows[0].can_start = false;
        uninstalled.observe(&rows);
        assert_eq!(
            uninstalled.claim_linked(server_id, false),
            Err(LinkedRefusal::Unavailable {
                server: name(),
                state: lookup("gui-pair-start-server-not-installed")
            })
        );

        let mut missing = ServerTools::default();
        assert!(matches!(
            missing.claim_linked(server_id, false),
            Err(LinkedRefusal::Unavailable { .. })
        ));
    }

    /// However a pair start ends, its settle frees the pair, and a settle for
    /// another start changes nothing.
    #[test]
    fn a_settled_pair_start_frees_the_pair_and_a_stale_settle_changes_nothing() {
        let ui = test_window();
        let (workspace, commands, client, _) = paired_workspace_starting_together(&ui, true);

        ui.invoke_launch_client(client.id().into());
        let first = starts(&commands)[0].2;
        apply_pair_start_settled(&ui, &workspace, &workspace.lifecycle, first + 100, false);
        assert!(workspace.lifecycle.borrow().pair_starting(client));

        apply_pair_start_settled(&ui, &workspace, &workspace.lifecycle, first, false);
        assert!(!workspace.lifecycle.borrow().pair_starting(client));

        ui.invoke_launch_client(client.id().into());
        let second = starts(&commands)[0].2;
        assert_ne!(second, first);
        apply_pair_start_settled(&ui, &workspace, &workspace.lifecycle, first, false);
        assert!(
            workspace.lifecycle.borrow().pair_starting(client),
            "an earlier start's settle does not end a later one"
        );
    }

    /// Restart stops and starts its own side, never the pair, and waits
    /// while the pair is starting.
    #[test]
    fn restart_never_starts_the_pair() {
        let ui = test_window();
        let (workspace, commands, client, server) = paired_workspace_starting_together(&ui, true);
        register_restart_actions(&ui, &workspace.commands, &workspace.lifecycle);

        ui.invoke_client_restart(client.id().into());
        ui.invoke_server_restart(server.id().into());
        let sent = starts(&commands);
        assert_eq!(
            sent.iter()
                .map(|(what, host, _)| (*what, *host))
                .collect::<Vec<_>>(),
            [("restart", client), ("restart", server)]
        );
        for (_, host, request) in sent {
            workspace.lifecycle.borrow_mut().apply(host, request, None);
        }

        ui.invoke_launch_client(client.id().into());
        let operation = starts(&commands)[0].2;
        ui.invoke_server_restart(server.id().into());
        assert!(
            starts(&commands).is_empty(),
            "no restart during a pair start"
        );
        apply_pair_start_settled(&ui, &workspace, &workspace.lifecycle, operation, false);
    }

    /// Every toast the window is asked to show, and whether it is an error.
    fn toasts(ui: &AppWindow) -> Rc<RefCell<Vec<(String, bool)>>> {
        let shown = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&shown);
        ui.on_present_toast(move |message, error| {
            sink.borrow_mut().push((message.to_string(), error));
        });
        shown
    }

    fn pair_commands(commands: &std::sync::mpsc::Receiver<Command>) -> usize {
        commands
            .try_iter()
            .filter(|command| {
                matches!(
                    command,
                    Command::SetLinkedPair { .. } | Command::ClearLinkedPair(_)
                )
            })
            .count()
    }

    #[test]
    fn showing_the_pair_from_a_single_pane_adds_it_beside() {
        let ui = test_window();
        let (workspace, _, client, server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Mods);
        assert!(!first_view(&ui).pair_shown);
        assert!(first_view(&ui).pair_room);

        ui.invoke_show_linked_pair(1, "server".into(), server.id().into());

        assert_eq!(
            shown(&workspace),
            [(client, Surface::Mods), (server, Surface::Overview)]
        );
        assert!(first_view(&ui).pair_shown);
    }

    #[test]
    fn each_side_of_a_pair_shows_the_state_of_the_other() {
        let ui = test_window();
        let (workspace, _, client, host) = paired_workspace(&ui);
        let InstalledHost::Server(server) = host else {
            unreachable!()
        };
        let (mut clients, servers) = paired_rows(client, host, false, false);
        clients[0].state = "Running".into();
        clients[0].state_kind = "running".into();
        workspace.state.borrow_mut().set_rows(clients, servers);
        workspace.open(&ui, client, Surface::Mods);
        ui.invoke_split_pane_beside(1);
        ui.invoke_choose_pane_host(2, "server".into(), host.id().into());
        let view_of = |host: InstalledHost| {
            ui.get_host_views()
                .iter()
                .find(|view| view.id == host.id().as_str())
                .unwrap()
        };
        assert_eq!(view_of(host).pair_state, "Running");
        assert_eq!(view_of(host).pair_state_kind, "running");

        apply_server_tool_event(
            &ui,
            &workspace,
            server,
            backend::ServerToolEvent::Finished {
                failure: Some("exit code 1".to_owned()),
            },
        );
        assert_eq!(view_of(host).server.state_kind, "failed");
        assert_eq!(view_of(client).pair_state, view_of(host).server.state);
        assert_eq!(view_of(client).pair_state_kind, "failed");

        assert!(workspace.lifecycle.borrow_mut().request(host).is_some());
        assert!(workspace.lifecycle.borrow_mut().request(client).is_some());
        stamp_host_lifecycle(&ui, &workspace.lifecycle.borrow());
        assert_eq!(view_of(client).pair_state_kind, "restarting");
        assert_eq!(view_of(client).pair_state, lookup("gui-state-restarting"));
        assert_eq!(view_of(host).pair_restart_stage, "stopping");
        assert_eq!(view_of(host).pair_state, "Running");
    }

    #[test]
    fn showing_the_pair_never_replaces_a_pane_of_a_full_workspace() {
        let ui = test_window();
        let toasts = toasts(&ui);
        let (workspace, _, client, server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Mods);
        ui.invoke_split_pane_beside(1);
        ui.invoke_split_pane_below(1);
        ui.invoke_split_pane_below(2);
        ui.invoke_show_pane_surface(2, "Logs".into());
        let before = shown(&workspace);
        let layout = workspace.state.borrow().layout().cloned();
        assert!(!first_view(&ui).pair_room);

        ui.invoke_show_linked_pair(4, "server".into(), server.id().into());

        assert_eq!(shown(&workspace), before);
        assert_eq!(workspace.state.borrow().layout().cloned(), layout);
        assert_eq!(
            toasts.borrow().as_slice(),
            [(
                lookup_args(
                    "gui-pair-no-room-for",
                    [("value", Arg::from("Repro Server"))]
                ),
                true
            )]
        );
    }

    #[test]
    fn showing_a_pair_already_on_screen_changes_nothing() {
        let ui = test_window();
        let toasts = toasts(&ui);
        let (workspace, _, client, server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Overview);
        ui.invoke_split_pane_beside(1);
        ui.invoke_choose_pane_host(2, "server".into(), server.id().into());
        ui.invoke_show_pane_surface(2, "Console".into());
        let layout = workspace.state.borrow().layout().cloned();

        ui.invoke_show_linked_pair(1, "server".into(), server.id().into());

        assert_eq!(workspace.state.borrow().layout().cloned(), layout);
        assert_eq!(toasts.borrow().len(), 1);
        assert!(!toasts.borrow()[0].1, "nothing went wrong");
    }

    /// Two panes on the client read one view, and moving panes around or
    /// leaving the workspace never touches the pair.
    #[test]
    fn the_pair_belongs_to_the_hosts_and_outlives_every_pane() {
        let ui = test_window();
        let (workspace, commands, client, server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Overview);
        ui.invoke_split_pane_beside(1);
        ui.invoke_show_pane_surface(2, "Logs".into());
        ui.invoke_show_linked_pair(1, "server".into(), server.id().into());

        let panes = ui.get_workspace_panes();
        let hosts: Vec<i32> = panes.iter().map(|pane| pane.host).collect();
        assert_eq!(hosts, [0, 0, 1], "both client panes read one view");
        let paired = |ui: &AppWindow| -> Vec<(String, String)> {
            ui.get_host_views()
                .iter()
                .map(|view| {
                    if view.kind == "client" {
                        (
                            view.id.to_string(),
                            view.client.paired_server_id.to_string(),
                        )
                    } else {
                        (
                            view.id.to_string(),
                            view.server.paired_client_id.to_string(),
                        )
                    }
                })
                .collect()
        };
        assert_eq!(
            paired(&ui),
            [(client.id(), server.id()), (server.id(), client.id())]
        );

        ui.invoke_close_pane(3);
        ui.invoke_choose_pane_host(2, "server".into(), server.id().into());
        ui.invoke_close_pane(2);
        workspace.close(&ui);
        workspace.open(&ui, client, Surface::Mods);

        assert_eq!(paired(&ui), [(client.id(), server.id())]);
        assert_eq!(
            pair_commands(&commands),
            0,
            "the layout never asks to unpair"
        );
    }

    #[test]
    fn pairing_and_unpairing_go_to_the_worker() {
        let ui = test_window();
        let (_workspace, commands, client, server) = paired_workspace(&ui);
        ui.invoke_set_linked_pair(client.id().into(), server.id().into(), true);
        ui.invoke_clear_linked_pair(client.id().into());

        let sent: Vec<Command> = commands.try_iter().collect();
        assert!(matches!(
            sent.as_slice(),
            [
                Command::SetLinkedPair {
                    client: set_client,
                    server: set_server,
                    start_together: true,
                },
                Command::ClearLinkedPair(cleared),
            ] if InstalledHost::Instance(*set_client) == client
                && InstalledHost::Server(*set_server) == server
                && InstalledHost::Instance(*cleared) == client
        ));
    }

    fn connects(commands: &std::sync::mpsc::Receiver<Command>) -> usize {
        commands
            .try_iter()
            .filter(|command| matches!(command, Command::LaunchAndConnect { .. }))
            .count()
    }

    /// Opens the server's console and has it show a line from an earlier run.
    fn console_with_earlier_output(
        workspace: &Workspace,
        ui: &AppWindow,
        server: InstalledHost,
    ) -> PaneId {
        workspace.open(ui, server, Surface::Console);
        let pane = workspace
            .state
            .borrow()
            .first_pane_showing(server)
            .expect("the console is open");
        deliver_server_line(workspace, server, "earlier run");
        assert!(console_text(workspace, pane).contains("earlier run"));
        pane
    }

    fn deliver_server_line(workspace: &Workspace, server: InstalledHost, text: &str) {
        let InstalledHost::Server(id) = server else {
            unreachable!()
        };
        let update = LiveLogUpdate {
            source: LiveLogSource::Server(id),
            generation: None,
            reset: false,
            missing: false,
            source_truncated: false,
            lines: vec![backend::LiveLogLine::Server(
                ephervex_server::CapturedLine::from_bytes(
                    ephervex_server::CaptureStream::Stdout,
                    text.as_bytes(),
                ),
            )],
        };
        let mut logs = workspace.logs.borrow_mut();
        let mut changes = HashMap::new();
        logs.apply(&update, &mut changes);
        logs.settle(changes);
    }

    fn console_text(workspace: &Workspace, pane: PaneId) -> String {
        workspace.logs.borrow().text(pane).unwrap_or_default()
    }

    #[test]
    fn launch_and_connect_starts_the_servers_console_over_with_the_new_run() {
        let ui = test_window();
        let (workspace, _commands, client, server) = paired_workspace(&ui);
        let console = console_with_earlier_output(&workspace, &ui, server);

        ui.invoke_launch_and_connect(client.id().into(), server.id().into());
        assert_eq!(console_text(&workspace, console), "");

        deliver_server_line(&workspace, server, "new run");
        let text = console_text(&workspace, console);
        assert!(text.contains("new run") && !text.contains("earlier run"));
    }

    #[test]
    fn a_console_the_start_itself_opens_asks_for_no_earlier_output() {
        let ui = test_window();
        let (workspace, commands, client, server) = paired_workspace_starting_together(&ui, true);
        commands.try_iter().for_each(drop);

        ui.invoke_launch_client(client.id().into());

        assert!(
            !commands.try_iter().any(|command| matches!(
                command,
                Command::OpenLiveLog {
                    source: LiveLogSource::Server(_),
                    ..
                }
            )),
            "a snapshot taken before the run writes can only be the run before"
        );
        deliver_server_line(&workspace, server, "new run");
        let pane = workspace
            .state
            .borrow()
            .first_pane_showing(server)
            .expect("the start shows the console");
        assert_eq!(console_text(&workspace, pane).trim(), "new run");
    }

    #[test]
    fn a_refused_pair_start_leaves_the_servers_console_free_to_read_its_capture() {
        let ui = test_window();
        let (workspace, commands, client, server) = paired_workspace_starting_together(&ui, true);
        let InstalledHost::Server(id) = server else {
            unreachable!()
        };
        ui.invoke_launch_client(client.id().into());
        let operation = workspace
            .lifecycle
            .borrow()
            .pair_starts
            .first()
            .map(|start| start.operation)
            .expect("the pair start is tracked");

        apply_pair_start_settled(&ui, &workspace, &workspace.lifecycle, operation, false);
        commands.try_iter().for_each(drop);
        workspace
            .logs
            .borrow_mut()
            .sync(&[(PaneId::from_raw(1), server, Surface::Logs)]);

        assert!(
            commands
                .try_iter()
                .any(|command| matches!(command, Command::OpenLiveLog { source: LiveLogSource::Server(opened), .. } if opened == id)),
            "the console reads the capture because no run is coming"
        );
    }

    #[test]
    fn restarting_a_server_starts_its_console_over_with_the_restarted_run() {
        let ui = test_window();
        let (workspace, _commands, _client, server) = paired_workspace(&ui);
        let console = console_with_earlier_output(&workspace, &ui, server);

        let lifecycle = Rc::clone(&workspace.lifecycle);
        apply_host_restart(
            &ui,
            &workspace,
            &lifecycle,
            server,
            0,
            Some(backend::RestartStage::Starting),
        );
        assert_eq!(console_text(&workspace, console), "");

        deliver_server_line(&workspace, server, "restarted run");
        let text = console_text(&workspace, console);
        assert!(text.contains("restarted run") && !text.contains("earlier run"));
    }

    #[test]
    fn a_restart_that_is_only_stopping_keeps_the_console() {
        let ui = test_window();
        let (workspace, _commands, _client, server) = paired_workspace(&ui);
        let console = console_with_earlier_output(&workspace, &ui, server);
        let lifecycle = Rc::clone(&workspace.lifecycle);

        apply_host_restart(
            &ui,
            &workspace,
            &lifecycle,
            server,
            1,
            Some(backend::RestartStage::Stopping),
        );
        assert!(console_text(&workspace, console).contains("earlier run"));
    }

    #[test]
    fn launching_a_client_that_starts_its_server_starts_the_console_over() {
        let ui = test_window();
        let (workspace, _commands, client, server) = paired_workspace_starting_together(&ui, true);
        let console = console_with_earlier_output(&workspace, &ui, server);

        ui.invoke_launch_client(client.id().into());
        assert_eq!(console_text(&workspace, console), "");
    }

    #[test]
    fn a_console_keeps_the_run_that_launch_and_connect_joins() {
        let ui = test_window();
        let (workspace, _commands, client, server) = paired_workspace(&ui);
        let console = console_with_earlier_output(&workspace, &ui, server);
        let (clients, rows) = paired_rows(client, server, false, true);
        workspace.lifecycle.borrow_mut().servers.observe(&rows);
        workspace.state.borrow_mut().set_rows(clients, rows);

        ui.invoke_launch_and_connect(client.id().into(), server.id().into());
        assert!(console_text(&workspace, console).contains("earlier run"));
    }

    #[test]
    fn a_console_keeps_the_run_that_a_client_launch_joins() {
        let ui = test_window();
        let (workspace, _commands, client, server) = paired_workspace_starting_together(&ui, true);
        let console = console_with_earlier_output(&workspace, &ui, server);
        let (clients, rows) = paired_rows(client, server, true, true);
        workspace.lifecycle.borrow_mut().servers.observe(&rows);
        workspace.state.borrow_mut().set_rows(clients, rows);

        ui.invoke_launch_client(client.id().into());
        assert!(console_text(&workspace, console).contains("earlier run"));
    }

    #[test]
    fn launch_and_connect_reuses_a_running_server_without_duplicate_submissions() {
        let ui = test_window();
        let (workspace, commands, client, host) = paired_workspace(&ui);
        let InstalledHost::Server(server) = host else {
            unreachable!()
        };
        let (clients, rows) = paired_rows(client, host, false, true);
        workspace.lifecycle.borrow_mut().servers.observe(&rows);
        workspace.state.borrow_mut().set_rows(clients, rows);
        ui.invoke_launch_and_connect(client.id().into(), host.id().into());
        ui.invoke_launch_and_connect(client.id().into(), host.id().into());
        ui.invoke_server_start(host.id().into());
        let sent = starts(&commands);
        assert_eq!(sent, [("connect", client, 0)]);
        assert!(!workspace.lifecycle.borrow().servers.busy(server));
        assert!(workspace.lifecycle.borrow().pair_starting(host));
    }

    #[test]
    fn launch_and_connect_while_its_pair_is_starting_says_so_and_sends_nothing() {
        let ui = test_window();
        let toasts = toasts(&ui);
        let (workspace, commands, client, host) = paired_workspace(&ui);
        let (clients, rows) = paired_rows(client, host, false, true);
        workspace.lifecycle.borrow_mut().servers.observe(&rows);
        workspace.state.borrow_mut().set_rows(clients, rows);

        ui.invoke_launch_and_connect(client.id().into(), host.id().into());
        assert!(toasts.borrow().is_empty(), "the first press is accepted");
        ui.invoke_launch_and_connect(client.id().into(), host.id().into());

        assert_eq!(starts(&commands), [("connect", client, 0)]);
        assert_eq!(
            toasts.borrow().as_slice(),
            [(lookup("gui-pair-start-busy"), true)]
        );
        assert!(workspace.lifecycle.borrow().pair_starting(host));
    }

    #[test]
    fn launch_and_connect_puts_the_server_beside_the_client() {
        let ui = test_window();
        let (workspace, commands, client, server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Mods);

        ui.invoke_launch_and_connect(client.id().into(), server.id().into());

        assert_eq!(connects(&commands), 1);
        assert_eq!(
            shown(&workspace),
            [(client, Surface::Mods), (server, Surface::Console)]
        );
    }

    #[test]
    fn launch_and_connect_from_the_server_puts_the_client_beside_it() {
        let ui = test_window();
        let (workspace, _, client, server) = paired_workspace(&ui);
        workspace.open(&ui, server, Surface::Console);

        ui.invoke_launch_and_connect(client.id().into(), server.id().into());

        assert_eq!(
            shown(&workspace),
            [(server, Surface::Console), (client, Surface::Overview)]
        );
    }

    #[test]
    fn launch_and_connect_with_no_workspace_open_opens_both_sides() {
        let ui = test_window();
        let (workspace, _, client, server) = paired_workspace(&ui);

        ui.invoke_launch_and_connect(client.id().into(), server.id().into());

        assert_eq!(
            shown(&workspace),
            [(client, Surface::Overview), (server, Surface::Console)]
        );
    }

    #[test]
    fn launch_and_connect_leaves_a_full_workspace_as_it_is() {
        let ui = test_window();
        let toasts = toasts(&ui);
        let (workspace, commands, client, server) = paired_workspace(&ui);
        workspace.open(&ui, client, Surface::Mods);
        ui.invoke_split_pane_beside(1);
        ui.invoke_split_pane_below(1);
        ui.invoke_split_pane_below(2);
        let layout = workspace.state.borrow().layout().cloned();

        ui.invoke_launch_and_connect(client.id().into(), server.id().into());

        assert_eq!(connects(&commands), 1, "the launch still goes ahead");
        assert_eq!(workspace.state.borrow().layout().cloned(), layout);
        assert_eq!(
            toasts.borrow().as_slice(),
            [(
                lookup_args(
                    "gui-pair-no-room-for",
                    [("value", Arg::from("Repro Server"))]
                ),
                false
            )]
        );
    }

    fn pane_labels(ui: &AppWindow) -> Vec<String> {
        ui.get_workspace_panes()
            .iter()
            .map(|pane| pane.label.to_string())
            .collect()
    }

    /// The pane controls change the shape one supported step at a time, and
    /// every pane is named by where it is, its host, and its surface.
    #[test]
    fn the_pane_controls_move_between_the_supported_shapes() {
        let ui = test_window();
        let (workspace, client, _) = listed_workspace(&ui);
        workspace.open(&ui, client, Surface::Isolation);
        assert_eq!(pane_labels(&ui), ["Repro Client, Isolation"]);

        ui.invoke_split_pane_beside(1);
        assert_eq!(
            shown(&workspace),
            [(client, Surface::Isolation), (client, Surface::Overview)],
            "the new pane shows the same host"
        );
        assert_eq!(
            pane_labels(&ui),
            [
                "Left pane: Repro Client, Isolation",
                "Right pane: Repro Client, Overview"
            ]
        );
        ui.invoke_split_pane_beside(1);
        assert_eq!(shown(&workspace).len(), 2, "there are only two columns");

        ui.invoke_split_pane_below(2);
        ui.invoke_split_pane_below(1);
        assert_eq!(
            workspace
                .state
                .borrow()
                .layout()
                .map(WorkspaceLayout::shape),
            Some(crate::workspace_layout::Shape::TwoByTwo)
        );
        assert_eq!(
            pane_labels(&ui),
            [
                "Top left pane: Repro Client, Isolation",
                "Bottom left pane: Repro Client, Overview",
                "Top right pane: Repro Client, Overview",
                "Bottom right pane: Repro Client, Overview"
            ]
        );
        let offered: Vec<(bool, bool, bool)> = ui
            .get_workspace_panes()
            .iter()
            .map(|pane| (pane.can_split_beside, pane.can_split_below, pane.can_close))
            .collect();
        assert_eq!(offered, [(false, false, true); 4]);

        for pane in [4, 3, 2] {
            ui.invoke_close_pane(pane);
        }
        assert_eq!(shown(&workspace), [(client, Surface::Isolation)]);
        assert!(
            ui.get_workspace_open(),
            "closing panes never leaves the workspace"
        );
        ui.invoke_close_pane(1);
        assert_eq!(
            shown(&workspace).len(),
            1,
            "the last pane is left with Back"
        );
    }

    /// A pane can show any installed host, keeping its surface when the host
    /// has it, and the host entering the workspace is loaded.
    #[test]
    fn a_pane_shows_whichever_installed_host_is_chosen() {
        let ui = test_window();
        let loaded = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&loaded);
        ui.on_load_server_workspace(move |id| sink.borrow_mut().push(id.to_string()));
        let (workspace, client, server) = listed_workspace(&ui);
        workspace.open(&ui, client, Surface::Logs);
        ui.invoke_split_pane_beside(1);
        ui.invoke_show_pane_surface(2, "Logs".into());

        let names = |choices: ModelRc<WorkspaceHostChoice>| -> Vec<String> {
            choices
                .iter()
                .map(|choice| choice.name.to_string())
                .collect()
        };
        assert_eq!(names(ui.get_workspace_client_choices()), ["Repro Client"]);
        assert_eq!(names(ui.get_workspace_server_choices()), ["Repro Server"]);

        ui.invoke_choose_pane_host(2, "server".into(), server.id().into());
        assert_eq!(
            shown(&workspace),
            [(client, Surface::Logs), (server, Surface::Logs)]
        );
        assert_eq!(loaded.borrow().as_slice(), [server.id()]);
        let panes: Vec<(i32, i32)> = ui
            .get_workspace_panes()
            .iter()
            .map(|pane| (pane.host, pane.surface_choice))
            .collect();
        let logs = |surfaces: &[Surface]| {
            surfaces
                .iter()
                .position(|&surface| surface == Surface::Logs)
                .and_then(|index| i32::try_from(index).ok())
                .expect("logs")
        };
        assert_eq!(
            panes,
            [(0, logs(Surface::client())), (1, logs(Surface::server()))]
        );
        assert_eq!(
            ui.get_workspace_back_label(),
            lookup("gui-back-instances").as_str(),
            "a split leaves for the page it was opened from"
        );

        ui.invoke_choose_pane_host(2, "client".into(), client.id().into());
        ui.invoke_show_pane_surface(2, "Overview".into());
        assert_eq!(ui.get_host_views().row_count(), 1);
    }

    /// The host picker lists running hosts first, a client ranked by what runs
    /// on its card, offers each client's linked servers on its card, and lists
    /// a linked server on its own only when the Servers page would.
    #[test]
    fn the_host_picker_lists_running_hosts_first_and_linked_servers_on_their_client() {
        let ui = test_window();
        let workspace = test_workspace(&ui);
        let client = |id: &str, running: bool| InstanceRow {
            id: id.into(),
            name: id.into(),
            can_kill: running,
            ..InstanceRow::default()
        };
        let server = |id: &str, linked: &str, running: bool| ServerRow {
            id: id.into(),
            name: id.into(),
            linked_client_id: linked.into(),
            can_stop: running,
            ..ServerRow::default()
        };
        let first = client_host();
        workspace.state.borrow_mut().set_rows(
            vec![
                client(&first.id(), false),
                client("idle", false),
                client("playing", true),
                client("both", true),
                client("quiet", false),
            ],
            vec![
                server("alone", "", false),
                server("linked", &first.id(), true),
                server("serving", "", true),
                server("both-server", "both", true),
                server("quiet-server", "quiet", false),
            ],
        );
        workspace.open(&ui, first, Surface::Overview);
        let names = |choices: ModelRc<WorkspaceHostChoice>| -> Vec<String> {
            choices
                .iter()
                .map(|choice| choice.name.to_string())
                .collect()
        };
        assert_eq!(
            names(ui.get_workspace_client_choices()),
            [
                "both".to_owned(),
                "playing".to_owned(),
                first.id(),
                "idle".to_owned(),
                "quiet".to_owned()
            ],
            "running client and server, running client, running server, the rest"
        );
        assert_eq!(
            names(ui.get_workspace_server_choices()),
            ["serving", "alone"]
        );
        let linked: Vec<String> = ui
            .get_workspace_client_choices()
            .row_data(2)
            .expect("the first client")
            .linked
            .iter()
            .map(|server| server.id.to_string())
            .collect();
        assert_eq!(linked, ["linked"]);

        ui.set_show_linked_servers(true);
        workspace.refresh(&ui);
        assert_eq!(
            names(ui.get_workspace_server_choices()),
            ["linked", "serving", "both-server", "alone", "quiet-server"]
        );
    }

    /// A restart asked for from either of two panes on one server is one
    /// restart, and both panes show it.
    #[test]
    fn a_restart_from_two_panes_on_one_host_is_one_restart() {
        let ui = test_window();
        let (workspace, _, server) = listed_workspace(&ui);
        let (sender, commands) = std::sync::mpsc::channel();
        register_restart_actions(&ui, &sender, &workspace.lifecycle);
        workspace.open(&ui, server, Surface::Console);
        ui.invoke_split_pane_beside(1);
        ui.invoke_show_pane_surface(2, "Logs".into());

        for _pane in [1, 2] {
            ui.invoke_server_restart(server.id().into());
        }
        let sent: Vec<Command> = commands.try_iter().collect();
        assert_eq!(sent.len(), 1, "the second pane's restart waits: {sent:?}");

        workspace.refresh(&ui);
        let panes: Vec<i32> = ui
            .get_workspace_panes()
            .iter()
            .map(|pane| pane.host)
            .collect();
        assert_eq!(panes, [0, 0], "both panes read the one host view");
        assert_eq!(
            ui.get_host_views()
                .row_data(0)
                .expect("view")
                .server
                .restart_stage,
            "stopping"
        );
    }

    /// Two panes on one host read one view of it, and whatever a feature
    /// reports about the host reaches both.
    #[test]
    fn a_host_in_two_panes_has_one_view_every_report_reaches() {
        let ui = test_window();
        let workspace = test_workspace(&ui);
        let host = client_host();
        workspace.open(&ui, host, Surface::Snapshots);
        let first = workspace.single_pane().expect("one pane");
        workspace.change_pane(&ui, first, |layout| layout.split_beside(first).is_ok());

        let views = ui.get_host_views();
        assert_eq!(views.row_count(), 1, "one host, one view");
        let panes = ui.get_workspace_panes();
        assert!(panes.iter().all(|pane| pane.host == 0));

        assert!(workspace.update(host, |view| view.snapshot_total = "2 snapshots".into()));
        assert_eq!(
            views.row_data(0).expect("view").snapshot_total,
            "2 snapshots"
        );
        assert!(
            !workspace.update(client_host(), |view| view.snapshot_total = "wrong".into()),
            "a report about a host no pane shows lands nowhere"
        );
        assert_eq!(
            views.row_data(0).expect("view").snapshot_total,
            "2 snapshots"
        );
    }

    /// A host no pane shows any more keeps nothing, so showing it again asks
    /// its features afresh rather than showing what they said before.
    #[test]
    fn a_host_leaving_the_workspace_forgets_what_was_reported() {
        let ui = test_window();
        let workspace = test_workspace(&ui);
        let (first, second) = (server_host(), server_host());
        workspace.open(&ui, first, Surface::Overview);
        workspace.update(first, |view| view.crash_report = "crash.txt".into());
        let pane = workspace.single_pane().expect("one pane");

        workspace.change_pane(&ui, pane, |layout| layout.show_host(pane, second).is_ok());
        assert!(!workspace.update(first, |view| view.crash_report = "late".into()));
        workspace.change_pane(&ui, pane, |layout| layout.show_host(pane, first).is_ok());

        assert_eq!(
            ui.get_host_views().row_data(0).expect("view").crash_report,
            ""
        );
    }

    /// Start is offered from four places. Every one of them has to leave the
    /// user watching the output the run is about to produce.
    #[test]
    fn showing_a_servers_console_opens_that_servers_console() {
        let ui = test_window();
        let workspace = test_workspace(&ui);
        let server = server_host();

        workspace.show_console(&ui, server);

        assert_eq!(shown(&workspace), [(server, Surface::Console)]);
        assert!(ui.get_workspace_open());
        assert_eq!(
            ui.get_page(),
            SERVERS_PAGE,
            "a single server's workspace belongs to the servers page"
        );
        let console = ui
            .get_log_views()
            .row_data(0)
            .expect("the console has a viewer");
        assert!(
            console.follow,
            "the output being waited for has not been written yet"
        );
        assert_eq!(console.mode, "live");
        assert_eq!(console.query, "");
    }

    /// Started from a client's workspace, Back returns to that client.
    #[test]
    fn starting_a_linked_server_keeps_its_clients_workspace_as_the_way_back() {
        let ui = test_window();
        let workspace = test_workspace(&ui);
        let (client, server) = (client_host(), server_host());
        workspace.open(&ui, client, Surface::Overview);

        workspace.show_console(&ui, server);

        assert_eq!(shown(&workspace), [(server, Surface::Console)]);
        assert_eq!(ui.get_page(), SERVERS_PAGE);
        assert_eq!(ui.get_workspace_back_label(), lookup("gui-back-instance"));
        ui.invoke_workspace_back();
        assert_eq!(shown(&workspace), [(client, Surface::Overview)]);
        assert_eq!(ui.get_page(), INSTANCES_PAGE);
        assert!(ui.get_workspace_open());
    }

    /// Started from the servers list there is no client to go back to, so
    /// Back leaves the workspace.
    #[test]
    fn starting_a_server_from_the_servers_list_has_no_way_back() {
        let ui = test_window();
        let workspace = test_workspace(&ui);

        workspace.show_console(&ui, server_host());
        assert_eq!(ui.get_workspace_back_label(), lookup("gui-back-servers"));
        ui.invoke_workspace_back();

        assert!(!ui.get_workspace_open());
        assert!(shown(&workspace).is_empty());
    }

    /// Started from inside the server workspace, whatever brought the user
    /// there still holds, including a client they reached it from.
    #[test]
    fn starting_a_server_from_its_own_workspace_keeps_the_way_back() {
        let ui = test_window();
        let workspace = test_workspace(&ui);
        let (client, server) = (client_host(), server_host());
        workspace.open(&ui, client, Surface::Overview);
        ui.invoke_follow_host(1, "server".into(), server.id().into(), "Overview".into());

        workspace.show_console(&ui, server);

        assert_eq!(shown(&workspace), [(server, Surface::Console)]);
        ui.invoke_workspace_back();
        assert_eq!(shown(&workspace), [(client, Surface::Overview)]);
    }

    /// A split is the user's own arrangement; starting a server from one of
    /// its panes does not rearrange it.
    #[test]
    fn starting_a_server_leaves_a_split_workspace_as_it_is() {
        let ui = test_window();
        let workspace = test_workspace(&ui);
        let (client, server) = (client_host(), server_host());
        workspace.open(&ui, client, Surface::Mods);
        let first = workspace.single_pane().expect("one pane");
        workspace.change_pane(&ui, first, |layout| layout.split_beside(first).is_ok());
        let before = shown(&workspace);

        workspace.show_console(&ui, server);

        assert_eq!(shown(&workspace), before);
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

    /// The overview tab shows every instance's own pending linked server, not
    /// only the one the user currently has open.
    #[test]
    fn linked_pending_servers_are_not_filtered_to_the_selected_instance() {
        let ui = test_window();
        let workspace = test_workspace(&ui);
        let (client_a, client_b) = (client_host(), client_host());
        workspace.open(&ui, client_a, Surface::Overview);
        let first_pane = workspace.single_pane().expect("one pane");
        workspace.change_pane(&ui, first_pane, |layout| {
            layout.split_beside(first_pane).is_ok()
        });
        ui.invoke_follow_host(2, "client".into(), client_b.id().into(), "Overview".into());

        let first = CreationKey::Server {
            name: "First Server".to_owned(),
            client: client_a.id(),
            minecraft: String::new(),
            loader: String::new(),
            loader_version: String::new(),
            persistent: false,
            port: None,
            server_pack: String::new(),
        };
        let mut second = first.clone();
        let CreationKey::Server { client, name, .. } = &mut second else {
            unreachable!()
        };
        *client = client_b.id();
        *name = "Second Server".to_owned();

        let mut tracker = CreationTracker::default();
        tracker.accept(first.clone(), pending_creation(&first, false));
        tracker.accept(second.clone(), pending_creation(&second, false));

        apply_pending_creations(&ui, &tracker);

        let linked_client_ids: Vec<String> = ui
            .get_linked_pending_servers()
            .iter()
            .map(|row| row.linked_client_id.to_string())
            .collect();
        assert_eq!(linked_client_ids.len(), 2);
        assert!(linked_client_ids.contains(&client_a.id()));
        assert!(
            linked_client_ids.contains(&client_b.id()),
            "an instance's pending linked server must show up on its own card \
             even while a different instance is selected"
        );

        // Each client's workspace view holds its own narrowed list, so a pane
        // renders only what it means to show rather than hiding the rest.
        let views = ui.get_host_views();
        assert_eq!(views.row_count(), 2);
        for view in views.iter() {
            let own: Vec<String> = view
                .pending_servers
                .iter()
                .map(|row| row.linked_client_id.to_string())
                .collect();
            assert_eq!(own, [view.id.to_string()]);
        }
    }

    fn disk_usage_report() -> ephervex_core::DiskUsageReport {
        use ephervex_core::{CategoryUsage, RetentionUsage};
        let category = |category, bytes: u64, locations: Vec<&str>| CategoryUsage {
            category,
            bytes,
            files: 1,
            locations: locations.into_iter().map(PathBuf::from).collect(),
        };
        let categories = StorageCategory::ALL
            .into_iter()
            .map(|which| match which {
                StorageCategory::ClientInstances => category(which, 600, vec!["/data/instances"]),
                StorageCategory::DownloadCache => {
                    category(which, 300, vec!["/cache/downloads", "/cache/metadata"])
                }
                StorageCategory::Recovery => category(which, 100, Vec::new()),
                other => category(other, 0, Vec::new()),
            })
            .collect();
        ephervex_core::DiskUsageReport {
            total_bytes: 1000,
            files: 3,
            categories,
            retention: vec![
                RetentionUsage {
                    retention: StorageRetention::Persistent,
                    bytes: 600,
                },
                RetentionUsage {
                    retention: StorageRetention::Temporary,
                    bytes: 0,
                },
                RetentionUsage {
                    retention: StorageRetention::Regenerable,
                    bytes: 300,
                },
                RetentionUsage {
                    retention: StorageRetention::Recovery,
                    bytes: 100,
                },
            ],
            unreadable: 2,
            unreadable_examples: vec![PathBuf::from("/data/instances/x/saves")],
            symlinks_skipped: 0,
            hard_links_deduplicated: 0,
            measured_at: chrono::Utc::now(),
        }
    }

    fn disk_usage_window() -> (
        AppWindow,
        std::sync::mpsc::Receiver<Command>,
        Rc<RefCell<DiskUsageState>>,
    ) {
        let ui = test_window();
        let (sender, commands) = std::sync::mpsc::channel();
        let state = Rc::new(RefCell::new(DiskUsageState::default()));
        register_disk_usage_actions(&ui, &sender, &state);
        (ui, commands, state)
    }

    fn modrinth_theme(accent: slint::Color) -> crate::theme_files::UserThemes {
        crate::theme_files::UserThemes {
            themes: vec![crate::theme_files::UserTheme {
                id: "modrinth".to_owned(),
                name: "Modrinth".to_owned(),
                parent: ephervex_core::ApplicationColorScheme::Dark,
                colors: vec![("accent", accent)],
                branding: crate::theme_files::ThemeBranding::default(),
            }],
            problems: vec!["broken.toml: Skipped.".to_owned()],
        }
    }

    /// The chosen theme, shown as it is.
    fn unseasoned(
        theme: &ephervex_core::ApplicationTheme,
    ) -> ephervex_core::seasonal_theme::ShownTheme {
        ephervex_core::seasonal_theme::ShownTheme::Recorded(theme.clone())
    }

    fn day(month: u32, day: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, month, day).expect("date")
    }

    #[test]
    fn a_season_paints_its_theme_while_the_page_keeps_the_chosen_one() {
        let ui = test_window();
        let themes = crate::theme_files::UserThemes::default();
        let mascot = crate::theme_files::read_selected_branding(
            std::path::Path::new("/nonexistent"),
            &mut crate::theme_files::UserThemes::default(),
            &ephervex_core::seasonal_theme::ShownTheme::Seasonal(
                ephervex_core::seasonal_theme::SeasonalTheme::Halloween,
            ),
        );
        let mut accents = Vec::new();
        for (chosen, label) in [
            (
                ephervex_core::ApplicationTheme::BuiltIn(
                    ephervex_core::ApplicationColorScheme::Dark,
                ),
                "gui-color-scheme-dark",
            ),
            (
                ephervex_core::ApplicationTheme::BuiltIn(
                    ephervex_core::ApplicationColorScheme::Light,
                ),
                "gui-color-scheme-light",
            ),
        ] {
            let shown = ephervex_core::seasonal_theme::shown_theme(&chosen, true, day(10, 25));
            apply_theme_settings(&ui, &chosen, true, &shown, &themes, &mascot, "");
            assert_eq!(ui.get_application_theme(), chosen.to_stored().as_str());
            let index = usize::try_from(ui.get_theme_index()).expect("an index");
            assert_eq!(
                ui.get_theme_ids().row_data(index).expect("a row"),
                chosen.to_stored().as_str(),
                "the list keeps the chosen theme"
            );
            assert_eq!(ui.get_shown_theme(), "season:halloween");
            assert!(ui.get_seasonal_themes());
            let note = ui.get_seasonal_theme_note();
            assert!(
                note.contains(&lookup(label))
                    && note.contains(&lookup("gui-seasonal-theme-halloween")),
                "{note}"
            );
            assert_eq!(ui.get_theme_logo().size().width, 620);
            accents.push(ui.global::<Theme>().get_accent());

            apply_theme_settings(
                &ui,
                &chosen,
                true,
                &unseasoned(&chosen),
                &themes,
                &crate::theme_files::BrandingImages::default(),
                "",
            );
            assert_eq!(ui.get_shown_theme(), chosen.to_stored().as_str());
            assert_eq!(ui.get_seasonal_theme_note(), "");
            assert_eq!(ui.get_theme_logo().size().width, 0, "the default logo");
            assert_ne!(ui.global::<Theme>().get_accent(), accents[0]);
        }
        assert_eq!(accents[0], accents[1], "one Halloween palette for both");
        let labels: Vec<SharedString> = ui.get_theme_labels().iter().collect();
        assert_eq!(
            labels,
            [
                lookup("gui-color-scheme-dark"),
                lookup("gui-color-scheme-light")
            ],
            "the Halloween theme is never offered"
        );
    }

    /// The first check after midnight on the first and the day after the last
    /// day of a season asks for the settings again, once, and no other check
    /// does.
    #[test]
    fn a_new_date_asks_for_the_settings_only_when_it_changes_the_shown_theme() {
        let mut watch = SeasonWatch::default();
        let mut shown = "light".to_owned();
        let mut asked = Vec::new();
        for (date, checks) in [
            (day(10, 24), 3),
            (day(10, 25), 3),
            (day(10, 31), 2),
            (day(11, 1), 3),
            (day(11, 2), 1),
        ] {
            for check in 0..checks {
                if watch.due("light", true, &shown, date) {
                    asked.push((date, check));
                    // The snapshot the refresh brings.
                    shown = ephervex_core::seasonal_theme::shown_theme(
                        &ephervex_core::ApplicationTheme::BuiltIn(
                            ephervex_core::ApplicationColorScheme::Light,
                        ),
                        true,
                        date,
                    )
                    .name();
                }
            }
        }
        assert_eq!(asked, [(day(10, 25), 0), (day(11, 1), 0)]);
    }

    /// A forced season holds on every date: the theme is asked for once when
    /// it is not yet shown, and no date change asks for another.
    #[test]
    fn a_forced_season_is_not_dropped_or_asked_for_again_as_the_date_changes() {
        let mut watch = SeasonWatch {
            forced: Some(ephervex_core::seasonal_theme::SeasonalTheme::Halloween),
            ..SeasonWatch::default()
        };
        assert!(watch.due("light", true, "light", day(3, 1)));
        for date in [day(3, 1), day(3, 2), day(10, 24), day(11, 1), day(11, 2)] {
            for _ in 0..3 {
                assert!(
                    !watch.due("light", true, "season:halloween", date),
                    "{date}"
                );
            }
        }
    }

    #[test]
    fn a_refresh_that_changes_nothing_is_not_asked_for_again_that_day() {
        let mut watch = SeasonWatch::default();
        assert!(watch.due("dark", true, "dark", day(10, 26)));
        assert!(!watch.due("dark", true, "dark", day(10, 26)));
        assert!(watch.due("dark", true, "dark", day(10, 27)));
    }

    /// Turning the switch off in season brings back the chosen theme.
    #[test]
    fn turning_seasonal_themes_off_in_season_asks_for_the_chosen_theme() {
        let mut watch = SeasonWatch::default();
        assert!(watch.due("light", false, "season:halloween", day(10, 28)));
    }

    #[test]
    fn a_user_theme_or_seasonal_themes_turned_off_never_asks_for_a_refresh() {
        let mut watch = SeasonWatch::default();
        for date in [day(10, 24), day(10, 25), day(10, 31), day(11, 1)] {
            assert!(!watch.due("user:modrinth", true, "user:modrinth", date));
            assert!(!watch.due("dark", false, "dark", date));
            assert!(!watch.due("light", false, "light", date));
        }
    }

    #[test]
    fn the_seasonal_switch_saves_on_its_own() {
        let ui = test_window();
        let (sender, commands) = std::sync::mpsc::channel();
        register_general_settings(&ui, &sender);

        ui.invoke_set_seasonal_themes(false);

        let sent: Vec<_> = commands.try_iter().collect();
        assert!(
            matches!(sent.as_slice(), [Command::SaveSeasonalThemes(false)]),
            "{sent:?}"
        );
    }

    #[test]
    fn the_theme_list_shows_user_themes_with_the_chosen_one_selected_and_painted() {
        let ui = test_window();
        let accent = slint::Color::from_argb_u8(255, 1, 2, 3);
        let chosen = ephervex_core::ApplicationTheme::User("modrinth".to_owned());

        apply_theme_settings(
            &ui,
            &chosen,
            true,
            &unseasoned(&chosen),
            &modrinth_theme(accent),
            &crate::theme_files::BrandingImages::default(),
            "/config/themes",
        );

        let labels: Vec<SharedString> = ui.get_theme_labels().iter().collect();
        assert_eq!(
            labels,
            [
                lookup("gui-color-scheme-dark"),
                lookup("gui-color-scheme-light"),
                "Modrinth".to_owned()
            ]
        );
        let ids: Vec<SharedString> = ui.get_theme_ids().iter().collect();
        assert_eq!(ids, ["dark", "light", "user:modrinth"]);
        assert_eq!(ui.get_theme_index(), 2);
        assert_eq!(ui.get_application_theme(), "user:modrinth");
        assert_eq!(ui.get_themes_folder(), "/config/themes");
        let problems: Vec<SharedString> = ui.get_theme_problems().iter().collect();
        assert_eq!(problems, ["broken.toml: Skipped."]);
        assert_eq!(ui.global::<Theme>().get_accent(), accent);
    }

    /// The chosen theme folder's decoded branding, read the way a settings
    /// snapshot reads it.
    fn branding_of(folder: &std::path::Path, id: &str) -> crate::theme_files::BrandingImages {
        let mut themes = crate::theme_files::read_user_themes(folder);
        let images = crate::theme_files::read_selected_branding(
            folder,
            &mut themes,
            &ephervex_core::seasonal_theme::ShownTheme::Recorded(
                ephervex_core::ApplicationTheme::User(id.to_owned()),
            ),
        );
        assert!(themes.problems.is_empty(), "{:?}", themes.problems);
        images
    }

    #[test]
    fn switching_themes_shows_each_themes_branding_and_returns_to_the_default() {
        let folder = tempfile::tempdir().expect("temp dir");
        for (id, rgba) in [
            ("orange", [255, 128, 0, 255]),
            ("violet", [128, 0, 255, 255]),
        ] {
            let branding = folder.path().join(id).join("branding");
            std::fs::create_dir_all(&branding).expect("package");
            std::fs::write(
                folder.path().join(id).join("theme.toml"),
                "[branding]\nlogo = \"logo.png\"\nicon = \"icon.png\"",
            )
            .expect("manifest");
            let png = ephervex_core::test_support::solid_png(6, 3, rgba);
            std::fs::write(branding.join("logo.png"), &png).expect("logo");
            std::fs::write(branding.join("icon.png"), &png).expect("icon");
        }
        let ui = test_window();
        let themes = crate::theme_files::read_user_themes(folder.path());
        let pixel = |image: slint::Image| {
            let buffer = image.to_rgba8().expect("pixels");
            let first = buffer.as_slice()[0];
            (
                buffer.width(),
                buffer.height(),
                [first.r, first.g, first.b, first.a],
            )
        };

        for (id, rgba) in [
            ("orange", [255, 128, 0, 255]),
            ("violet", [128, 0, 255, 255]),
            ("orange", [255, 128, 0, 255]),
        ] {
            let chosen = ephervex_core::ApplicationTheme::User(id.to_owned());
            apply_theme_settings(
                &ui,
                &chosen,
                true,
                &unseasoned(&chosen),
                &themes,
                &branding_of(folder.path(), id),
                "",
            );
            assert_eq!(pixel(ui.get_theme_logo()), (6, 3, rgba), "{id} logo");
            // The square icon's top-left corner is padding.
            assert_eq!(
                pixel(ui.get_theme_icon()),
                (6, 6, [0, 0, 0, 0]),
                "{id} icon"
            );
        }

        apply_theme_settings(
            &ui,
            &ephervex_core::ApplicationTheme::default(),
            true,
            &unseasoned(&ephervex_core::ApplicationTheme::default()),
            &themes,
            &crate::theme_files::BrandingImages::default(),
            "",
        );
        assert_eq!(ui.get_theme_logo().size().width, 0, "the default logo");
        assert_eq!(ui.get_theme_icon().size().width, 0, "the default icon");
    }

    #[test]
    fn choosing_a_user_theme_saves_it_as_a_user_theme() {
        let ui = test_window();
        let (sender, commands) = std::sync::mpsc::channel();
        register_general_settings(&ui, &sender);

        ui.invoke_save_general_settings(100, 12, "user:modrinth".into(), false, true, 4);

        let sent: Vec<_> = commands.try_iter().collect();
        let [Command::SaveGeneralSettings { theme, .. }] = sent.as_slice() else {
            panic!("one save: {sent:?}");
        };
        assert_eq!(
            *theme,
            ephervex_core::ApplicationTheme::User("modrinth".to_owned())
        );
    }

    #[test]
    fn measuring_asks_once_and_shows_progress_until_it_settles() {
        let (ui, commands, state) = disk_usage_window();
        ui.invoke_measure_disk_usage();
        ui.invoke_measure_disk_usage();
        let sent: Vec<_> = commands.try_iter().collect();
        assert_eq!(sent.len(), 1, "a second click waits: {sent:?}");
        let Command::MeasureDiskUsage { request } = sent[0] else {
            panic!("a measurement");
        };
        assert_eq!(ui.get_disk_usage_state(), "measuring");

        settle_disk_usage(&ui, &mut state.borrow_mut(), request);
        assert_eq!(
            ui.get_disk_usage_state(),
            "idle",
            "stopped before any report, so nothing is claimed"
        );
        ui.invoke_measure_disk_usage();
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::MeasureDiskUsage { .. })
        ));
    }

    #[test]
    fn a_report_becomes_a_total_proportions_and_largest_first_categories() {
        let (ui, commands, state) = disk_usage_window();
        ui.invoke_measure_disk_usage();
        let Ok(Command::MeasureDiskUsage { request }) = commands.try_recv() else {
            panic!("a measurement");
        };
        apply_disk_usage(&ui, &mut state.borrow_mut(), request, &disk_usage_report());

        assert_eq!(ui.get_disk_usage_state(), "measured");
        assert_eq!(
            ui.get_disk_usage_total(),
            ephervex_model::format_binary_bytes(1000).as_str()
        );
        let retention: Vec<(String, f32)> = ui
            .get_disk_usage_retention()
            .iter()
            .map(|row| (row.id.to_string(), row.fraction))
            .collect();
        assert_eq!(
            retention,
            [
                ("persistent".to_owned(), 0.6),
                ("temporary".to_owned(), 0.0),
                ("regenerable".to_owned(), 0.3),
                ("recovery".to_owned(), 0.1),
            ]
        );
        let categories: Vec<String> = ui
            .get_disk_usage_categories()
            .iter()
            .map(|row| row.id.to_string())
            .collect();
        assert_eq!(
            categories.len(),
            StorageCategory::ALL.len(),
            "every category listed"
        );
        assert_eq!(
            &categories[..3],
            ["client_instances", "download_cache", "recovery"]
        );
        let cache = ui.get_disk_usage_categories().row_data(1).expect("row");
        assert_eq!(cache.locations.row_count(), 2);
        assert_eq!(
            cache.label,
            lookup("gui-disk-usage-download-cache").as_str()
        );
        assert!(ui.get_disk_usage_incomplete());
        assert_eq!(ui.get_disk_usage_unreadable().row_count(), 1);
    }

    #[test]
    fn a_report_for_an_earlier_request_is_not_shown() {
        let (ui, commands, state) = disk_usage_window();
        ui.invoke_measure_disk_usage();
        let Ok(Command::MeasureDiskUsage { request }) = commands.try_recv() else {
            panic!("a measurement");
        };
        apply_disk_usage(
            &ui,
            &mut state.borrow_mut(),
            request + 1,
            &disk_usage_report(),
        );
        assert_eq!(ui.get_disk_usage_state(), "measuring");
    }

    #[test]
    fn a_category_folder_is_opened_by_category_and_position() {
        let (ui, commands, _state) = disk_usage_window();
        ui.invoke_open_storage_location("download_cache".into(), 1);
        ui.invoke_open_storage_location("not-a-category".into(), 0);
        let sent: Vec<_> = commands.try_iter().collect();
        assert_eq!(sent.len(), 1, "an unknown category opens nothing: {sent:?}");
        assert!(matches!(
            sent[0],
            Command::OpenStorageLocation {
                category: StorageCategory::DownloadCache,
                index: 1
            }
        ));
    }

    #[test]
    fn stopping_a_measurement_cancels_its_reported_operation() {
        let (ui, commands, state) = disk_usage_window();
        ui.invoke_measure_disk_usage();
        let _ = commands.try_recv();
        let activity = ActivityData {
            item: crate::presentation::ActivityPresentation {
                id: "operation-9".to_owned(),
                title: lookup("operation-measuring-disk-usage"),
                status: String::new(),
                status_kind: "running",
                detail: "/cache".to_owned(),
                progress: 0.0,
                indeterminate: true,
                cancellable: true,
                failed: false,
            },
            entity: String::new(),
            time: String::new(),
        };
        apply_disk_usage_activity(&ui, &mut state.borrow_mut(), &activity);
        assert_eq!(ui.get_disk_usage_progress(), "/cache");
        ui.invoke_stop_disk_usage();
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::CancelOperation(operation)) if operation == "operation-9"
        ));
    }

    fn counted(id: &str, count: i64) -> String {
        lookup_args(id, [("count", Arg::from(count))])
    }

    fn storage_event(event: backend::StorageMaintenanceEvent) -> Event {
        Event::StorageMaintenance(Box::new(event))
    }

    fn apply_storage(
        ui: &AppWindow,
        state: &Rc<RefCell<DiskUsageState>>,
        commands: &std::sync::mpsc::Sender<Command>,
        event: backend::StorageMaintenanceEvent,
    ) {
        let Event::StorageMaintenance(event) = storage_event(event) else {
            unreachable!("built just above");
        };
        storage_maintenance::apply(ui, &mut state.borrow_mut(), *event, commands);
    }

    fn storage_usage(entries: u64, bytes: u64) -> ephervex_core::Usage {
        ephervex_core::Usage {
            entries,
            files: entries,
            bytes,
        }
    }

    fn inspection() -> ephervex_core::StorageInspection {
        use ephervex_core::{ManagedStorageGroup, ManagedStorageKind, WithheldReason};
        ephervex_core::StorageInspection {
            groups: ManagedStorageKind::ALL
                .into_iter()
                .map(|kind| ManagedStorageGroup {
                    kind,
                    required: if kind == ManagedStorageKind::Libraries {
                        storage_usage(3, 3 * 1024 * 1024)
                    } else {
                        ephervex_core::Usage::default()
                    },
                    unused: if kind == ManagedStorageKind::DownloadedFiles {
                        storage_usage(2, 2048)
                    } else {
                        ephervex_core::Usage::default()
                    },
                    withheld: if kind == ManagedStorageKind::InstallerFiles {
                        vec![(
                            WithheldReason::LoaderFilesNotRecorded,
                            storage_usage(1, 512),
                        )]
                    } else {
                        Vec::new()
                    },
                })
                .collect(),
            clients: 2,
            servers: 1,
            complete: true,
            inspected_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn each_maintenance_step_asks_once_and_is_shown_as_running_until_it_settles() {
        let (ui, commands, state) = disk_usage_window();
        ui.invoke_inspect_storage();
        ui.invoke_verify_storage();
        ui.invoke_review_storage_cleanup();
        let sent: Vec<_> = commands.try_iter().collect();
        assert_eq!(sent.len(), 1, "a second step waits for the first: {sent:?}");
        assert!(matches!(sent[0], Command::InspectStorage));
        assert_eq!(ui.get_storage_maintenance_busy(), "inspect");

        let (sender, _unused) = std::sync::mpsc::channel();
        apply_storage(
            &ui,
            &state,
            &sender,
            backend::StorageMaintenanceEvent::Settled,
        );
        assert_eq!(ui.get_storage_maintenance_busy(), "");
        ui.invoke_verify_storage();
        assert!(matches!(commands.try_recv(), Ok(Command::VerifyStorage)));
        ui.invoke_recover_storage();
        assert!(commands.try_recv().is_err(), "still verifying");
    }

    #[test]
    fn an_inspection_lists_every_kind_with_what_is_in_use_unused_and_kept() {
        let (ui, _commands, state) = disk_usage_window();
        let (sender, _unused) = std::sync::mpsc::channel();
        apply_storage(
            &ui,
            &state,
            &sender,
            backend::StorageMaintenanceEvent::Inspected(inspection()),
        );

        assert!(ui.get_storage_inspected());
        assert_eq!(
            ui.get_storage_headline(),
            lookup_args(
                "gui-storage-reclaimable",
                [(
                    "value",
                    Arg::from(ephervex_model::format_binary_bytes(2048))
                )]
            )
            .as_str()
        );
        let rows: Vec<_> = ui.get_storage_groups().iter().collect();
        assert_eq!(rows.len(), ephervex_core::ManagedStorageKind::ALL.len());
        assert_eq!(rows[0].id, "downloaded_files");
        assert_eq!(
            rows[0].size,
            ephervex_model::format_binary_bytes(2048).as_str()
        );
        assert_eq!(
            rows[0].lines.iter().collect::<Vec<_>>(),
            [SharedString::from(
                lookup_args(
                    "gui-storage-line-unused",
                    [(
                        "value",
                        Arg::from(ephervex_model::format_binary_bytes(2048))
                    )]
                )
                .as_str()
            )]
        );
        let installers: Vec<String> = rows[1].lines.iter().map(|line| line.to_string()).collect();
        assert_eq!(installers.len(), 1);
        assert!(
            installers[0].contains(&lookup("gui-storage-withheld-loader")),
            "a kept group says why: {installers:?}"
        );
        assert_eq!(
            rows[1].size, "0 B",
            "kept storage is not offered as reclaimable"
        );
        assert!(!ui.get_storage_incomplete());
    }

    #[test]
    fn a_reviewed_plan_opens_the_review_and_the_confirmation_names_that_plan() {
        use ephervex_core::{CleanupItem, KeptReason, KeptStorage, ManagedStorageKind};
        let (ui, commands, state) = disk_usage_window();
        let plan = ephervex_core::test_support::cleanup_plan_for_review(
            vec![CleanupItem {
                kind: ManagedStorageKind::Libraries,
                usage: storage_usage(2, 4096),
                largest: vec![PathBuf::from("libraries/org/old/old.jar")],
            }],
            vec![KeptStorage {
                kind: ManagedStorageKind::AssetObjects,
                reason: KeptReason::Required,
                usage: storage_usage(5, 10_000),
            }],
            false,
        );
        let (sender, _unused) = std::sync::mpsc::channel();
        apply_storage(
            &ui,
            &state,
            &sender,
            backend::StorageMaintenanceEvent::Planned {
                plan: 7,
                preview: plan,
            },
        );

        assert_eq!(ui.get_modal(), "storage-cleanup");
        assert_eq!(ui.get_storage_plan_id(), 7);
        assert!(!ui.get_storage_plan_empty());
        assert!(
            ui.get_storage_plan_incomplete(),
            "a partial listing says so"
        );
        assert_eq!(
            ui.get_storage_plan_confirm(),
            lookup_args(
                "gui-storage-cleanup-confirm",
                [(
                    "value",
                    Arg::from(ephervex_model::format_binary_bytes(4096))
                )]
            )
            .as_str()
        );
        let item = ui.get_storage_plan_items().row_data(0).expect("an item");
        assert_eq!(
            item.size,
            ephervex_model::format_binary_bytes(4096).as_str()
        );
        assert_eq!(
            item.detail,
            lookup("gui-storage-why-libraries").as_str(),
            "the review says why it is disposable"
        );
        assert_eq!(item.examples.row_count(), 1);
        assert_eq!(ui.get_storage_plan_kept().row_count(), 1);

        ui.invoke_confirm_storage_cleanup(ui.get_storage_plan_id());
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::ApplyStorageCleanup { plan: 7 })
        ));
        assert_eq!(ui.get_storage_maintenance_busy(), "clean");
    }

    #[test]
    fn a_cleanup_has_no_way_to_start_without_a_plan_id() {
        let (ui, commands, _state) = disk_usage_window();
        ui.invoke_confirm_storage_cleanup(-1);
        assert!(commands.try_recv().is_err());
        assert_eq!(ui.get_storage_maintenance_busy(), "");
    }

    #[test]
    fn a_cleanup_and_a_recovery_take_the_old_totals_and_results_off_the_screen() {
        let (ui, commands, state) = disk_usage_window();
        ui.invoke_measure_disk_usage();
        let Ok(Command::MeasureDiskUsage { request }) = commands.try_recv() else {
            panic!("a measurement");
        };
        apply_disk_usage(&ui, &mut state.borrow_mut(), request, &disk_usage_report());
        settle_disk_usage(&ui, &mut state.borrow_mut(), request);
        let (sender, receiver) = std::sync::mpsc::channel();
        apply_storage(
            &ui,
            &state,
            &sender,
            backend::StorageMaintenanceEvent::Inspected(inspection()),
        );
        assert_eq!(ui.get_disk_usage_state(), "measured");

        apply_storage(
            &ui,
            &state,
            &sender,
            backend::StorageMaintenanceEvent::Cleaned(ephervex_core::CleanupOutcome {
                planned: storage_usage(1, 100),
                removed: storage_usage(1, 100),
                ..ephervex_core::CleanupOutcome::default()
            }),
        );

        assert_eq!(
            ui.get_disk_usage_state(),
            "idle",
            "stale totals are not shown"
        );
        assert!(ui.get_disk_usage_stale());
        assert!(!ui.get_storage_inspected(), "nor is the old inspection");
        assert_eq!(ui.get_storage_result().row_count(), 1);
        assert!(
            matches!(receiver.try_recv(), Ok(Command::MeasureArtifactCache)),
            "the storage page's own figure is measured again"
        );

        ui.invoke_measure_disk_usage();
        assert!(
            !ui.get_disk_usage_stale(),
            "measuring again clears the notice"
        );
    }

    #[test]
    fn a_measurement_still_running_when_the_disk_changes_is_discarded() {
        let (ui, commands, state) = disk_usage_window();
        ui.invoke_measure_disk_usage();
        let Ok(Command::MeasureDiskUsage { request }) = commands.try_recv() else {
            panic!("a measurement");
        };
        let (sender, _unused) = std::sync::mpsc::channel();
        apply_storage(
            &ui,
            &state,
            &sender,
            backend::StorageMaintenanceEvent::Cleaned(ephervex_core::CleanupOutcome::default()),
        );

        apply_disk_usage(&ui, &mut state.borrow_mut(), request, &disk_usage_report());

        assert_eq!(
            ui.get_disk_usage_state(),
            "measuring",
            "a figure read while the disk was changing is not shown, and the run is still a run"
        );
        settle_disk_usage(&ui, &mut state.borrow_mut(), request);
        assert_eq!(ui.get_disk_usage_state(), "idle");
        assert!(ui.get_disk_usage_stale());
        ui.invoke_measure_disk_usage();
        assert!(
            matches!(commands.try_recv(), Ok(Command::MeasureDiskUsage { .. })),
            "measuring again works once the discarded run has ended"
        );
    }

    #[test]
    fn a_recovery_shows_what_is_still_damaged_and_why_a_version_failed() {
        use ephervex_core::{
            IntegrityReport, ManagedStorageKind, RecoveryReport, StorageFault, StorageFaultKind,
        };
        let fault = |path: &str, needed: bool| StorageFault {
            kind: ManagedStorageKind::Libraries,
            path: PathBuf::from(path),
            label: path.to_owned(),
            fault: StorageFaultKind::Corrupt,
            standing: if needed {
                ephervex_core::FaultStanding::Needed
            } else {
                ephervex_core::FaultStanding::Unused
            },
        };
        let still = fault("libraries/a.jar", true);
        let (ui, _commands, state) = disk_usage_window();
        let (sender, _unused) = std::sync::mpsc::channel();
        apply_storage(
            &ui,
            &state,
            &sender,
            backend::StorageMaintenanceEvent::Recovered {
                report: RecoveryReport {
                    recovered: vec![fault("libraries/b.jar", true)],
                    still_damaged: vec![still.clone()],
                    left_for_cleanup: vec![fault("downloads/c.jar", false)],
                    left_in_place: Vec::new(),
                    failures: Vec::new(),
                    canceled: false,
                    after: IntegrityReport {
                        checked: 4,
                        faults: vec![still, fault("downloads/c.jar", false)],
                        unchecked: Vec::new(),
                        complete: true,
                        verified_at: chrono::Utc::now(),
                    },
                },
                failures: vec![("1.21.1".to_owned(), "the download failed".to_owned())],
            },
        );

        let result: Vec<String> = ui
            .get_storage_result()
            .iter()
            .map(|line| line.to_string())
            .collect();
        assert!(
            result.contains(&counted("gui-storage-recovery-recovered", 1)),
            "{result:?}"
        );
        assert!(result.contains(&counted("gui-storage-recovery-still-damaged", 1)));
        assert!(result.contains(&counted("gui-storage-recovery-leftover", 1)));
        assert!(
            result
                .iter()
                .any(|line| line.contains("1.21.1") && line.contains("the download failed")),
            "the provider failure is named: {result:?}"
        );
        assert!(ui.get_storage_checked());
        assert_eq!(ui.get_storage_faults().row_count(), 2);
        assert!(
            ui.get_storage_can_recover(),
            "the file that is still damaged can be tried again"
        );
        assert!(ui.get_storage_integrity().contains('2'));
    }

    #[test]
    fn stopping_maintenance_cancels_the_operation_it_reported() {
        let (ui, commands, state) = disk_usage_window();
        ui.invoke_verify_storage();
        let _ = commands.try_recv();
        let activity = ActivityData {
            item: crate::presentation::ActivityPresentation {
                id: "operation-12".to_owned(),
                title: lookup("operation-verifying-storage"),
                status: String::new(),
                status_kind: "running",
                detail: "a.jar".to_owned(),
                progress: 0.0,
                indeterminate: true,
                cancellable: true,
                failed: false,
            },
            entity: String::new(),
            time: String::new(),
        };
        apply_disk_usage_activity(&ui, &mut state.borrow_mut(), &activity);
        assert_eq!(ui.get_storage_maintenance_progress(), "a.jar");
        ui.invoke_stop_storage_maintenance();
        assert!(matches!(
            commands.try_recv(),
            Ok(Command::CancelOperation(operation)) if operation == "operation-12"
        ));
    }

    #[test]
    fn a_confirmation_for_a_replaced_review_tells_the_user_to_review_again() {
        let (ui, _commands, state) = disk_usage_window();
        let (sender, _unused) = std::sync::mpsc::channel();
        apply_storage(
            &ui,
            &state,
            &sender,
            backend::StorageMaintenanceEvent::PlanOutdated,
        );
        apply_storage(
            &ui,
            &state,
            &sender,
            backend::StorageMaintenanceEvent::Settled,
        );
        assert_eq!(ui.get_storage_maintenance_busy(), "");
    }

    /// A window listing one client and one server, as the lists and the
    /// workspaces all read them.
    fn restart_window() -> (
        AppWindow,
        std::sync::mpsc::Receiver<Command>,
        Rc<RefCell<HostLifecycle>>,
        ephervex_model::InstanceId,
        ephervex_model::ServerId,
    ) {
        let ui = test_window();
        let client = ephervex_model::InstanceId::new();
        let server = ephervex_model::ServerId::new();
        ui.set_instances(model(vec![InstanceRow {
            id: client.to_string().into(),
            can_kill: true,
            ..InstanceRow::default()
        }]));
        let server_row = ServerRow {
            id: server.to_string().into(),
            can_restart: true,
            ..ServerRow::default()
        };
        ui.set_global_servers(model(vec![server_row]));
        let (sender, commands) = std::sync::mpsc::channel();
        let lifecycle = Rc::new(RefCell::new(HostLifecycle::default()));
        register_restart_actions(&ui, &sender, &lifecycle);
        (ui, commands, lifecycle, client, server)
    }

    #[test]
    fn a_restart_is_claimed_per_host_and_a_second_request_waits() {
        let (ui, commands, _lifecycle, client, server) = restart_window();
        ui.invoke_client_restart(client.to_string().into());
        ui.invoke_client_restart(client.to_string().into());
        ui.invoke_server_restart(server.to_string().into());
        ui.invoke_server_restart(server.to_string().into());

        let sent: Vec<InstalledHost> = commands
            .try_iter()
            .map(|command| match command {
                Command::RestartHost { host, .. } => host,
                other => panic!("a restart: {other:?}"),
            })
            .collect();
        assert_eq!(
            sent,
            [
                InstalledHost::Instance(client),
                InstalledHost::Server(server)
            ],
            "one restart per host, and hosts do not block each other"
        );
    }

    #[test]
    fn every_row_showing_a_host_carries_its_restart_stage() {
        let (ui, commands, lifecycle, _client, server) = restart_window();
        ui.set_host_views(model(vec![HostView {
            kind: "server".into(),
            server: ui.get_global_servers().row_data(0).expect("row"),
            ..HostView::default()
        }]));
        ui.invoke_server_restart(server.to_string().into());
        let Ok(Command::RestartHost { host, request }) = commands.try_recv() else {
            panic!("a restart");
        };
        let stages = |ui: &AppWindow| -> Vec<String> {
            [
                ui.get_global_servers().row_data(0).expect("row"),
                ui.get_host_views().row_data(0).expect("view").server,
            ]
            .iter()
            .map(|row| row.restart_stage.to_string())
            .collect()
        };
        assert_eq!(stages(&ui), ["stopping", "stopping"]);

        // The server reports its own move to starting, without a request.
        assert!(
            lifecycle
                .borrow_mut()
                .apply(host, 0, Some(backend::RestartStage::Starting))
        );
        stamp_host_lifecycle(&ui, &lifecycle.borrow());
        assert_eq!(stages(&ui), ["starting", "starting"]);

        assert!(
            !lifecycle.borrow_mut().apply(host, request + 1, None),
            "an ending for some other restart changes nothing"
        );
        assert!(!lifecycle.borrow_mut().apply(host, 0, None));
        assert!(lifecycle.borrow_mut().apply(host, request, None));
        stamp_host_lifecycle(&ui, &lifecycle.borrow());
        assert_eq!(stages(&ui), ["", ""]);

        ui.invoke_server_restart(server.to_string().into());
        assert!(
            matches!(commands.try_recv(), Ok(Command::RestartHost { .. })),
            "a restart that ended frees the host for the next"
        );
    }

    #[test]
    fn a_client_is_restartable_only_while_its_launch_is_known() {
        let (ui, _commands, lifecycle, client, _server) = restart_window();
        let can_restart = |ui: &AppWindow| ui.get_instances().row_data(0).expect("row").can_restart;
        assert!(!can_restart(&ui));

        lifecycle
            .borrow_mut()
            .restartable
            .insert(client.to_string());
        stamp_host_lifecycle(&ui, &lifecycle.borrow());
        assert!(can_restart(&ui));

        // A snapshot rebuilds rows from records that know nothing of this.
        ui.set_instances(model(vec![InstanceRow {
            id: client.to_string().into(),
            can_kill: true,
            ..InstanceRow::default()
        }]));
        stamp_host_lifecycle(&ui, &lifecycle.borrow());
        assert!(can_restart(&ui), "stamped again after a refresh");
    }

    /// The overview reports the work on the row of the instance it belongs to,
    /// so the rows have to be told about it, and told again when it ends.
    #[test]
    fn an_instances_row_carries_its_own_linked_servers_progress() {
        let ui = test_window();
        let rows = vec![
            InstanceRow {
                id: "client-a".into(),
                ..InstanceRow::default()
            },
            InstanceRow {
                id: "client-b".into(),
                ..InstanceRow::default()
            },
        ];
        ui.set_instances(model(rows));

        let building = CreationKey::Server {
            name: "First Server".to_owned(),
            client: "client-a".to_owned(),
            minecraft: String::new(),
            loader: String::new(),
            loader_version: String::new(),
            persistent: false,
            port: None,
            server_pack: String::new(),
        };
        let mut tracker = CreationTracker::default();
        tracker.accept(building.clone(), pending_creation(&building, false));
        apply_pending_creations(&ui, &tracker);

        let carried: Vec<String> = ui
            .get_instances()
            .iter()
            .map(|row| row.creating_server_detail.to_string())
            .collect();
        assert_eq!(
            carried.len(),
            2,
            "both rows are written, including the one with nothing to say"
        );
        assert!(
            !carried[0].is_empty(),
            "the instance being built for has to say what its server is doing"
        );
        assert!(
            carried[1].is_empty(),
            "an unrelated instance must not report somebody else's work"
        );

        // And when the work ends, the row it was on goes back to saying
        // nothing rather than keeping the last thing it said.
        apply_pending_creations(&ui, &CreationTracker::default());
        assert!(
            ui.get_instances()
                .iter()
                .all(|row| row.creating_server_detail.is_empty())
        );
    }

    fn import_key(archive: &str) -> CreationKey {
        CreationKey::Import {
            archive: archive.to_owned(),
        }
    }

    fn start_import(tracker: &mut CreationTracker, archive: &str) -> String {
        let key = import_key(archive);
        tracker
            .accept(key.clone(), pending_creation(&key, false))
            .expect("a new archive is accepted")
    }

    fn pending_instance_ids(ui: &AppWindow) -> Vec<String> {
        ui.get_pending_instances()
            .iter()
            .map(|row| row.id.to_string())
            .collect()
    }

    /// Once an import has created its instance, the remaining linked-server
    /// work is that instance's, so the import's own card must not stay beside
    /// it as a second entry.
    #[test]
    fn an_import_creating_its_linked_server_stops_showing_a_card_of_its_own() {
        let ui = test_window();
        ui.set_show_linked_servers(true);
        let mut tracker = CreationTracker::default();
        let request = start_import(&mut tracker, "/packs/gta.mrpack");
        apply_pending_creations(&ui, &tracker);
        assert_eq!(pending_instance_ids(&ui), std::slice::from_ref(&request));

        ui.set_instances(model(vec![InstanceRow {
            id: "client-a".into(),
            ..InstanceRow::default()
        }]));
        tracker.hand_off_to_linked_server(&request, "client-a", "GTA Server");
        apply_pending_creations(&ui, &tracker);

        assert!(
            pending_instance_ids(&ui).is_empty(),
            "the instance exists, so no second top-level entry is left"
        );
        let row = ui.get_instances().row_data(0).expect("instance row");
        assert_eq!(
            row.creating_server_detail,
            lookup("gui-creating-linked-server"),
            "the instance's row says its server is still being prepared"
        );
        assert!(row.creating_server_indeterminate);
    }

    #[test]
    fn linked_server_progress_after_an_import_follows_the_instance_it_belongs_to() {
        let ui = test_window();
        let mut tracker = CreationTracker::default();
        let request = start_import(&mut tracker, "/packs/gta.mrpack");
        ui.set_instances(model(vec![
            InstanceRow {
                id: "client-a".into(),
                ..InstanceRow::default()
            },
            InstanceRow {
                id: "client-b".into(),
                ..InstanceRow::default()
            },
        ]));
        tracker.hand_off_to_linked_server(&request, "client-a", "GTA Server");
        tracker.progressed(
            &request,
            "Installing the linked server | 1.18.2 server_mappings".to_owned(),
            0.4,
            false,
        );
        apply_pending_creations(&ui, &tracker);

        let rows: Vec<_> = ui.get_instances().iter().collect();
        assert_eq!(
            rows[0].creating_server_detail,
            "Installing the linked server | 1.18.2 server_mappings"
        );
        assert!((rows[0].creating_server_progress - 0.4).abs() < f32::EPSILON);
        assert!(
            rows[1].creating_server_detail.is_empty(),
            "an unrelated instance carries nothing of this import"
        );
    }

    /// A linked server that fails or is canceled ends the import's request the
    /// same way a success does, and nothing is left on the row.
    #[test]
    fn a_finished_import_leaves_neither_a_card_nor_progress_on_its_instance() {
        let ui = test_window();
        let mut tracker = CreationTracker::default();
        let request = start_import(&mut tracker, "/packs/gta.mrpack");
        ui.set_instances(model(vec![InstanceRow {
            id: "client-a".into(),
            ..InstanceRow::default()
        }]));
        tracker.hand_off_to_linked_server(&request, "client-a", "GTA Server");
        apply_pending_creations(&ui, &tracker);
        assert!(
            !ui.get_instances()
                .row_data(0)
                .expect("row")
                .creating_server_detail
                .is_empty()
        );

        tracker.finish(&request);
        apply_pending_creations(&ui, &tracker);

        assert!(pending_instance_ids(&ui).is_empty());
        assert_eq!(ui.get_linked_pending_servers().row_count(), 0);
        assert!(
            ui.get_instances()
                .row_data(0)
                .expect("row")
                .creating_server_detail
                .is_empty()
        );
        assert!(
            tracker
                .accept(
                    import_key("/packs/gta.mrpack"),
                    pending_creation(&import_key("/packs/gta.mrpack"), false)
                )
                .is_some(),
            "the archive can be imported again"
        );
    }

    #[test]
    fn an_import_without_a_linked_server_keeps_its_card_until_it_finishes() {
        let ui = test_window();
        let mut tracker = CreationTracker::default();
        let request = start_import(&mut tracker, "/packs/plain.mrpack");
        apply_pending_creations(&ui, &tracker);
        assert_eq!(pending_instance_ids(&ui), std::slice::from_ref(&request));
        assert_eq!(ui.get_linked_pending_servers().row_count(), 0);

        tracker.finish(&request);
        apply_pending_creations(&ui, &tracker);
        assert!(pending_instance_ids(&ui).is_empty());
    }

    #[test]
    fn concurrent_imports_hand_off_independently() {
        let ui = test_window();
        let mut tracker = CreationTracker::default();
        let first = start_import(&mut tracker, "/packs/first.mrpack");
        let second = start_import(&mut tracker, "/packs/second.mrpack");
        ui.set_instances(model(vec![InstanceRow {
            id: "client-a".into(),
            ..InstanceRow::default()
        }]));
        tracker.hand_off_to_linked_server(&first, "client-a", "First Server");
        apply_pending_creations(&ui, &tracker);

        assert_eq!(
            pending_instance_ids(&ui),
            std::slice::from_ref(&second),
            "the other import keeps its own card"
        );
        tracker.progressed(&second, "Resolving".to_owned(), 0.5, false);
        apply_pending_creations(&ui, &tracker);
        assert_ne!(
            ui.get_instances()
                .row_data(0)
                .expect("row")
                .creating_server_detail,
            "Resolving",
            "the second import's progress does not reach the first instance"
        );
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
            ui_markup(),
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
        ui_declaration(source, "export component AppWindow inherits Window {")
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.strip_prefix("    callback "))
            .filter_map(|rest| rest.split('(').next())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn every_callback_the_window_declares_reaches_the_rust_side() {
        let declared = window_callbacks(ui_markup());
        assert!(
            declared.len() > 50,
            "the parser found {} callbacks, so it is no longer reading the window component",
            declared.len()
        );

        let rust = [
            include_str!("controller.rs"),
            include_str!("controller/diagnosis_flow.rs"),
            include_str!("controller/java_runtime.rs"),
            include_str!("controller/log_views.rs"),
            include_str!("controller/storage_maintenance.rs"),
            include_str!("controller/workspace.rs"),
            include_str!("main.rs"),
        ];
        for name in declared {
            let handler = format!("on_{}(", name.replace('-', "_"));
            assert!(
                rust.iter().any(|source| source.contains(&handler)),
                "`{name}` is declared on the window and nothing handles it, so the control \
                 that invokes it does nothing"
            );
        }
    }

    #[test]
    fn modrinth_search_does_not_reuse_the_server_filter() {
        let source = ui_markup();
        assert!(source.contains("root.search(root.host.browser-provider, self.text)"));
        assert!(source.contains("text <=> root.server-query"));
        assert!(!source.contains("root.server-query)"));
    }

    /// What a Properties save of a running server sends after the form was
    /// loaded with `stored` and the user made `edits`, while the server turned
    /// its allowlist on behind the form.
    fn values_sent_by_a_running_save(
        stored: &[(&str, &str)],
        edits: &[(&str, &str)],
    ) -> Vec<(String, String)> {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_server_actions(&ui, &workspace.commands, &workspace);
        let server = ephervex_model::ServerId::new();
        let row = ServerRow {
            id: server.to_string().into(),
            state_kind: "running".into(),
            ..ServerRow::default()
        };
        workspace.state.borrow_mut().set_rows(Vec::new(), vec![row]);
        workspace.open(&ui, InstalledHost::Server(server), Surface::Properties);
        apply_server_workspace(
            &workspace,
            ServerWorkspaceData {
                server: server.to_string(),
                values: stored
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
                awaiting_restart: std::collections::BTreeSet::new(),
                lists_every_key: true,
                network: ephervex_core::ServerNetworkSettings::default(),
            },
        );
        for (key, value) in edits {
            ui.invoke_edit_server_property(
                server.to_string().into(),
                (*key).into(),
                (*value).into(),
            );
        }

        ui.invoke_save_server_properties(
            server.to_string().into(),
            false,
            "Localhost".into(),
            SharedString::default(),
            "25565".into(),
        );
        commands
            .try_iter()
            .find_map(|command| match command {
                Command::SaveServerProperties { values, .. } => Some(values),
                _ => None,
            })
            .expect("a save")
    }

    #[test]
    fn the_defaults_page_saves_the_stored_defaults_with_its_edits() {
        let ui = test_window();
        let (sender, commands) = std::sync::mpsc::channel();
        let drafts = Rc::new(RefCell::new(SettingsDrafts::default()));
        register_server_defaults(&ui, &sender, &drafts);
        drafts.borrow_mut().server_defaults.load(
            [("motd", "Repro"), ("pvp", "false")]
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
        );

        ui.invoke_edit_server_default("view-distance".into(), "16".into());
        ui.invoke_reset_server_default("motd".into());
        ui.invoke_choose_server_default("difficulty".into(), 1);
        assert_eq!(ui.get_server_defaults_edits(), 3);
        ui.invoke_save_server_defaults();

        let saved = commands
            .try_iter()
            .find_map(|command| match command {
                Command::SaveServerPropertyDefaults(values) => Some(values),
                _ => None,
            })
            .expect("a save");
        let expected: std::collections::BTreeMap<String, String> = [
            ("difficulty", "peaceful"),
            ("pvp", "false"),
            ("view-distance", "16"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        assert_eq!(saved, expected);
    }

    #[test]
    fn a_save_sends_only_the_properties_changed_since_the_form_was_loaded() {
        let stored = [
            ("motd", "before"),
            ("max-players", "20"),
            ("white-list", "false"),
        ];
        assert_eq!(
            values_sent_by_a_running_save(&stored, &[("motd", "after")]),
            [("motd".to_owned(), "after".to_owned())]
        );
    }

    #[test]
    fn a_save_that_corrects_a_stored_player_count_sends_only_what_changed() {
        let stored = [("max-players", "0"), ("white-list", "false")];
        assert_eq!(
            values_sent_by_a_running_save(&stored, &[("max-players", "20")]),
            [("max-players".to_owned(), "20".to_owned())]
        );
    }

    #[test]
    fn a_save_with_a_value_its_key_refuses_sends_nothing() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_server_actions(&ui, &workspace.commands, &workspace);
        let server = ephervex_model::ServerId::new();
        workspace.state.borrow_mut().set_rows(
            Vec::new(),
            vec![ServerRow {
                id: server.to_string().into(),
                ..ServerRow::default()
            }],
        );
        workspace.open(&ui, InstalledHost::Server(server), Surface::Properties);
        apply_server_workspace(
            &workspace,
            ServerWorkspaceData {
                server: server.to_string(),
                values: std::collections::BTreeMap::new(),
                awaiting_restart: std::collections::BTreeSet::new(),
                lists_every_key: false,
                network: ephervex_core::ServerNetworkSettings::default(),
            },
        );
        ui.invoke_edit_server_property(
            server.to_string().into(),
            "view-distance".into(),
            "99".into(),
        );
        ui.invoke_save_server_properties(
            server.to_string().into(),
            false,
            "Localhost".into(),
            SharedString::default(),
            "25565".into(),
        );
        assert!(
            commands
                .try_iter()
                .all(|command| !matches!(command, Command::SaveServerProperties { .. }))
        );
    }

    #[test]
    fn creation_forms_use_catalog_models_for_exact_versions() {
        let source = ui_markup();
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

        let source = ui_markup();
        assert!(source.contains("root.set-default-account(account.id)"));
        assert!(source.contains("root.remove-account(account.id)"));
        assert!(!source.contains("root.set-default-account(account.name)"));
        assert!(!source.contains("root.remove-account(account.name)"));
    }

    /// The markup outside the `Theme` global and the palettes it reads.
    ///
    /// Those are the one place a color or a text size is written. Anywhere
    /// else, a literal is a token nobody named and a palette nobody checked.
    fn markup_outside_the_theme() -> String {
        let source = crate::test_support::UI_SOURCES
            .iter()
            .filter(|(path, _)| *path != "palettes.slint")
            .map(|(_, source)| *source)
            .collect::<Vec<_>>()
            .join("\n");
        let theme = ui_declaration(&source, "export global Theme {")
            .expect("the markup declares the theme");
        source.replacen(theme, "", 1)
    }

    /// Every color the interface paints with is named in `Theme`, so another
    /// palette is a matter of giving each name a value rather than of finding
    /// the places that never asked for one.
    #[test]
    fn every_interface_color_is_named_in_the_theme_global() {
        let markup = markup_outside_the_theme();
        // Comments may quote a color they are explaining.
        let markup: String = markup
            .lines()
            .map(|line| line.split("//").next().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        for written in ["rgb(", "rgba(", "hsv(", "oklch("] {
            assert!(
                !markup.contains(written),
                "`{written}` writes a color the theme does not name"
            );
        }
        let hex: Vec<&str> = markup
            .split('#')
            .skip(1)
            .map(|rest| {
                let end = rest
                    .find(|c: char| !c.is_ascii_hexdigit())
                    .unwrap_or(rest.len());
                &rest[..end]
            })
            .filter(|candidate| matches!(candidate.len(), 3 | 4 | 6 | 8))
            .collect();
        assert!(hex.is_empty(), "colors written in the markup: {hex:?}");
    }

    /// The shipped widgets paint from their own palette, so a window that
    /// repaints itself and leaves that palette alone puts a light ComboBox on
    /// a dark page.
    #[test]
    fn the_widget_palette_follows_the_interface_scheme() {
        let source = ui_markup();
        assert!(source.contains("Palette.color-scheme = self.widget-scheme"));
        assert!(source.contains("ListView, Palette, ScrollView"));
    }

    /// Closing a stand-in question answers it and nothing more. The answer
    /// already shows whichever prompt waited behind it, so a close that went on
    /// to clear the dialog would hide that prompt while its operation waits.
    #[test]
    fn closing_a_stand_in_question_leaves_the_prompt_behind_it_on_screen() {
        let markup = ui_markup();
        assert!(
            markup.contains(
                "else if root.modal == \"mod-stand-in\" { root.answer-mod-stand-in(root.mod-stand-in-id, false); } else {"
            ),
            "the close button answers the question without clearing the dialog"
        );
    }

    #[test]
    fn declining_a_stand_in_question_shows_the_manual_download_prompt_behind_it() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        register_stand_in_answers(&ui, &workspace.commands, &workspace);
        let host = ephervex_model::InstanceId::new().to_string();
        apply_mod_stand_in(&ui, stand_in_prompt(1), &workspace);
        apply_mod_acquisition(&ui, acquisition_prompt(2, &host), &workspace);

        ui.invoke_answer_mod_stand_in("1".into(), false);
        end_mod_stand_in(&ui, 1, &workspace);

        assert_eq!(ui.get_modal(), "mod-acquire");
        assert_eq!(ui.get_mod_acquisition_id(), "2");
    }

    /// The message ids the markup passes to one `GuiI18n` callback by literal.
    fn markup_message_ids(callback: &str) -> Vec<&'static str> {
        let markup = ui_markup();
        let call = format!("GuiI18n.{callback}(\"");
        markup
            .match_indices(&call)
            .filter_map(|(at, _)| {
                let rest = &markup[at + call.len()..];
                rest.find('"').map(|end| &rest[..end])
            })
            .collect()
    }

    /// A message the markup formats through `GuiI18n.value`, `.count`, or
    /// `.values` must read the argument names that bridge supplies, or the
    /// window shows the unfilled placeable.
    #[test]
    fn interface_messages_read_the_arguments_the_bridge_supplies() {
        let bridges: [(&str, &[&str]); 3] = [
            ("value", &["value"]),
            ("count", &["count"]),
            ("values", &["first", "second"]),
        ];
        for (callback, names) in bridges {
            let ids = markup_message_ids(callback);
            assert!(!ids.is_empty(), "no `{callback}` calls found in the markup");
            for id in ids {
                assert!(has(id), "`{id}` is not a message");
                let text = ephervex_i18n::lookup_args(
                    id,
                    names.iter().map(|name| {
                        let value = if *name == "count" {
                            Arg::from(7_usize)
                        } else {
                            Arg::from(format!("<{name}>"))
                        };
                        (*name, value)
                    }),
                );
                assert!(
                    !text.contains("{$"),
                    "`{id}` leaves a placeable open: {text}"
                );
            }
        }
    }

    #[test]
    fn mod_clash_progress_shows_the_position_in_the_clash_list() {
        let text = ephervex_i18n::lookup_args(
            "gui-mod-clash-progress",
            [("first", Arg::from("1")), ("second", Arg::from("3"))],
        );
        assert!(text.contains('1') && text.contains("of") && text.contains('3'));
        assert!(!text.contains("{$"), "{text}");
        assert!(markup_message_ids("values").contains(&"gui-mod-clash-progress"));
    }

    /// Interface text follows the size the user chose, which it can only do
    /// while every `font-size` reads a `Theme` token. The separate size the
    /// diagnostics and console views use is the one exception, and it is
    /// written from a property rather than as a literal.
    #[test]
    fn every_interface_text_size_follows_the_typography_scale() {
        let markup = markup_outside_the_theme();
        let literals: Vec<&str> = markup
            .match_indices("font-size:")
            .map(|(at, _)| markup[at..].lines().next().unwrap_or_default())
            .filter(|line| {
                line.split("font-size:")
                    .nth(1)
                    .and_then(|rest| rest.trim_start().chars().next())
                    .is_some_and(|first| first.is_ascii_digit())
            })
            .collect();
        assert!(literals.is_empty(), "text sizes written out: {literals:#?}");
    }

    /// `AppButton` asks for room for its whole caption and elides whatever it
    /// is not given. The console toolbar is the one row where a pinned width
    /// cannot be checked by eye, because its captions grow with the separately
    /// chosen diagnostics text size, so nothing on it pins one.
    #[test]
    fn no_control_on_the_console_toolbar_is_pinned_to_a_width() {
        let source = ui_markup();
        let start = source
            .find("gui-search-retained-logs")
            .expect("the console toolbar starts at its search field");
        let end = source[start..]
            .find("gui-new-log-lines")
            .expect("and ends at its pending-line count")
            + start;
        let pinned: Vec<&str> = source[start..end]
            .lines()
            .filter(|line| line.contains("min-width:"))
            .collect();
        assert!(pinned.is_empty(), "controls pinned to a width: {pinned:#?}");
    }

    #[test]
    fn context_menus_expose_entity_actions_without_redundant_inspector_deletion() {
        let source = ui_markup();
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
        assert!(source.matches("PopupMenu {").count() >= 3);
        assert!(
            !source.contains("ContextMenuArea"),
            "the toolkit's own menu takes no color from a theme"
        );
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
                        blocked: false,
                        selected: true,
                    })
                    .collect(),
            },
            ..backend::ModPlanData::default()
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

    fn stored_settings(root: &str, memory_min: u32, path: &str) -> SettingsData {
        SettingsData {
            persistent_root: root.to_owned(),
            java_memory_min: memory_min,
            java_memory_max: 4096,
            java_preferred_path: path.to_owned(),
            microsoft_client_id: String::new(),
            user_agent: String::new(),
            artifact_cache_custom_value: "500".to_owned(),
            artifact_cache_custom_unit: "GiB".to_owned(),
            ..SettingsData::default()
        }
    }

    #[test]
    fn an_unrelated_snapshot_keeps_unsaved_settings_input() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().expect("window");
        let mut drafts = SettingsDrafts::default();
        apply_settings_fields(&ui, &stored_settings("/data", 512, ""), &mut drafts);

        ui.set_java_memory_min("1024".into());
        ui.set_java_preferred_path("/opt/java".into());
        ui.set_persistent_root("/elsewhere".into());
        ui.set_java_auto_detect(!ui.get_java_auto_detect());
        ui.set_user_agent("custom/1".into());
        ui.set_artifact_cache_custom_value("750".into());
        let detect = ui.get_java_auto_detect();
        apply_settings_fields(&ui, &stored_settings("/data", 512, ""), &mut drafts);

        assert_eq!(ui.get_java_memory_min(), "1024");
        assert_eq!(ui.get_java_preferred_path(), "/opt/java");
        assert_eq!(ui.get_persistent_root(), "/elsewhere");
        assert_eq!(ui.get_java_auto_detect(), detect);
        assert_eq!(ui.get_user_agent(), "custom/1");
        assert_eq!(ui.get_artifact_cache_custom_value(), "750");
    }

    #[test]
    fn untouched_settings_fields_follow_the_stored_values() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().expect("window");
        let mut drafts = SettingsDrafts::default();
        apply_settings_fields(&ui, &stored_settings("/data", 512, ""), &mut drafts);
        ui.set_java_memory_max("8192".into());

        apply_settings_fields(
            &ui,
            &stored_settings("/moved", 768, "/usr/bin/java"),
            &mut drafts,
        );

        assert_eq!(ui.get_persistent_root(), "/moved");
        assert_eq!(ui.get_java_memory_min(), "768");
        assert_eq!(ui.get_java_preferred_path(), "/usr/bin/java");
        // The one field that was edited stays as typed.
        assert_eq!(ui.get_java_memory_max(), "8192");
    }

    #[test]
    fn a_completed_save_shows_the_stored_form_of_what_was_typed() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().expect("window");
        let mut drafts = SettingsDrafts::default();
        apply_settings_fields(&ui, &stored_settings("/data", 512, ""), &mut drafts);

        ui.set_persistent_root("  /data/new  ".into());
        ui.set_java_memory_min("0768".into());
        drafts.persistent_root.submit("  /data/new  ".to_owned());
        drafts.java_memory_min.submit("0768".to_owned());
        apply_settings_fields(&ui, &stored_settings("/data/new", 768, ""), &mut drafts);
        assert_eq!(ui.get_persistent_root(), "  /data/new  ");

        finish_settings_save(&ui, &mut drafts, SettingsSave::StorageRoot, true);
        finish_settings_save(&ui, &mut drafts, SettingsSave::JavaSettings, true);

        assert_eq!(ui.get_persistent_root(), "/data/new");
        assert_eq!(ui.get_java_memory_min(), "768");
        // Later snapshots follow the stored value again.
        apply_settings_fields(&ui, &stored_settings("/data/later", 768, ""), &mut drafts);
        assert_eq!(ui.get_persistent_root(), "/data/later");
    }

    #[test]
    fn text_typed_while_a_save_runs_is_not_replaced_when_it_completes() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().expect("window");
        let mut drafts = SettingsDrafts::default();
        apply_settings_fields(&ui, &stored_settings("/data", 512, ""), &mut drafts);

        ui.set_java_preferred_path("/opt/a".into());
        drafts.java_preferred_path.submit("/opt/a".to_owned());
        ui.set_java_preferred_path("/opt/b".into());
        apply_settings_fields(&ui, &stored_settings("/data", 512, "/opt/a"), &mut drafts);
        finish_settings_save(&ui, &mut drafts, SettingsSave::JavaSettings, true);

        assert_eq!(ui.get_java_preferred_path(), "/opt/b");
    }

    #[test]
    fn a_refused_save_leaves_the_typed_input_in_place() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().expect("window");
        let mut drafts = SettingsDrafts::default();
        apply_settings_fields(&ui, &stored_settings("/data", 512, ""), &mut drafts);

        ui.set_persistent_root("/not/allowed".into());
        drafts.persistent_root.submit("/not/allowed".to_owned());
        finish_settings_save(&ui, &mut drafts, SettingsSave::StorageRoot, false);
        apply_settings_fields(&ui, &stored_settings("/data", 512, ""), &mut drafts);
        apply_settings_fields(&ui, &stored_settings("/data", 512, ""), &mut drafts);

        assert_eq!(ui.get_persistent_root(), "/not/allowed");
    }

    fn settings_with_channel() -> (
        AppWindow,
        std::sync::mpsc::Receiver<Command>,
        Sender<Command>,
        Rc<RefCell<SettingsDrafts>>,
    ) {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().expect("window");
        let (sender, receiver) = std::sync::mpsc::channel();
        let drafts = Rc::new(RefCell::new(SettingsDrafts::default()));
        register_settings(&ui, &sender, &drafts);
        (ui, receiver, sender, drafts)
    }

    fn finish(ui: &AppWindow, drafts: &Rc<RefCell<SettingsDrafts>>, save: SettingsSave, ok: bool) {
        let mut held = drafts.borrow_mut();
        finish_settings_save(ui, &mut held, save, ok);
        show_settings_saving(ui, &held);
    }

    #[test]
    fn a_second_settings_save_is_not_sent_while_the_first_is_running() {
        let (ui, commands, _sender, drafts) = settings_with_channel();

        ui.invoke_save_storage_settings("/data".into());
        assert!(ui.get_storage_root_saving());
        ui.invoke_save_storage_settings("/data".into());
        ui.invoke_reset_storage_root();
        assert_eq!(commands.try_iter().count(), 1);

        finish(&ui, &drafts, SettingsSave::StorageRoot, true);
        assert!(!ui.get_storage_root_saving());
        ui.invoke_save_storage_settings("/data".into());
        assert_eq!(commands.try_iter().count(), 1);
    }

    #[test]
    fn a_failed_settings_save_releases_its_buttons() {
        let (ui, commands, _sender, drafts) = settings_with_channel();

        ui.invoke_save_java_settings(true, false, "512".into(), "2048".into(), "".into());
        assert!(ui.get_java_saving());
        finish(&ui, &drafts, SettingsSave::JavaSettings, false);
        assert!(!ui.get_java_saving());

        ui.invoke_save_artifact_cache_limit(0, "".into(), "GiB".into());
        assert!(ui.get_artifact_cache_limit_saving());
        finish(&ui, &drafts, SettingsSave::ArtifactCacheLimit, false);
        assert!(!ui.get_artifact_cache_limit_saving());
        assert_eq!(commands.try_iter().count(), 2);
    }

    #[test]
    fn a_refused_java_memory_range_does_not_leave_the_save_running() {
        let (ui, commands, _sender, _drafts) = settings_with_channel();

        ui.invoke_save_java_settings(true, false, "9000".into(), "1000".into(), "".into());

        assert!(!ui.get_java_saving());
        assert_eq!(commands.try_iter().count(), 0);
    }

    #[test]
    fn the_api_save_waits_for_both_identifier_saves_to_finish() {
        let (ui, commands, _sender, drafts) = settings_with_channel();

        ui.invoke_save_api_settings("id".into(), "agent".into());
        assert!(ui.get_microsoft_client_id_saving() && ui.get_user_agent_saving());
        ui.invoke_reset_microsoft_client_id();
        ui.invoke_reset_user_agent();
        ui.invoke_save_api_settings("id".into(), "agent".into());
        assert_eq!(commands.try_iter().count(), 1);

        finish(&ui, &drafts, SettingsSave::MicrosoftClientId, true);
        assert!(!ui.get_microsoft_client_id_saving() && ui.get_user_agent_saving());
        ui.invoke_save_api_settings("id".into(), "agent".into());
        assert_eq!(commands.try_iter().count(), 0);

        finish(&ui, &drafts, SettingsSave::UserAgent, true);
        assert!(!ui.get_user_agent_saving());
        ui.invoke_reset_user_agent();
        assert_eq!(commands.try_iter().count(), 1);
    }

    #[test]
    fn a_settings_save_the_worker_never_received_does_not_leave_buttons_disabled() {
        let (ui, commands, sender, _drafts) = settings_with_channel();
        drop(commands);

        ui.invoke_save_storage_settings("/data".into());

        assert!(!ui.get_storage_root_saving());
        assert!(take_worker_stopped());
        drop(sender);
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

    /// Diagnostics names a missing mod, not a release. Its install shows the
    /// resolved plan, exact release included, and installs nothing until that
    /// is confirmed, even when the plan leaves nothing else to decide. The
    /// browser's own button still installs such a plan directly.
    #[test]
    fn a_diagnostics_install_is_confirmed_before_anything_installs() {
        let installs = |commands: &std::sync::mpsc::Receiver<Command>| {
            commands
                .try_iter()
                .filter_map(|command| match command {
                    Command::InstallMod {
                        project,
                        version,
                        confirmed,
                        ..
                    } => Some((project, version, confirmed)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for from_diagnostics in [true, false] {
            let ui = test_window();
            let (workspace, commands) = test_workspace_sending(&ui);
            register_browser_install(&ui, &workspace.commands, &workspace);
            register_mod_mutations(&ui, &workspace.commands, &workspace);
            let instance = ephervex_model::InstanceId::new().to_string();
            if from_diagnostics {
                ui.invoke_install_from_diagnostics(
                    "client".into(),
                    instance.as_str().into(),
                    "modrinth".into(),
                    "utilitix".into(),
                );
            } else {
                ui.invoke_install_from_browser(
                    "client".into(),
                    instance.as_str().into(),
                    "modrinth".into(),
                    "utilitix".into(),
                );
            }
            assert!(
                commands
                    .try_iter()
                    .any(|command| matches!(command, Command::PlanMod { .. })),
                "the release is resolved first"
            );

            let mut resolved = plan(1, 0);
            resolved.instance.clone_from(&instance);
            resolved.linked.instance.clone_from(&instance);
            apply_mod_install_plan(&ui, resolved, &workspace, &workspace.commands);

            if from_diagnostics {
                assert_eq!(ui.get_modal(), "mod-plan");
                assert_eq!(ui.get_mod_plan_version(), "21.1.6");
                assert!(installs(&commands).is_empty(), "nothing before confirming");

                // What the confirmation's Install button sends.
                ui.invoke_install_mod(
                    ui.get_mod_plan_instance(),
                    ui.get_mod_plan_provider(),
                    ui.get_mod_plan_project(),
                    ui.get_mod_plan_version_id(),
                );
                assert_eq!(
                    installs(&commands),
                    [("utilitix".to_owned(), "abcd1234".to_owned(), true)]
                );
            } else {
                assert_eq!(ui.get_modal(), "");
                assert_eq!(
                    installs(&commands),
                    [("utilitix".to_owned(), "abcd1234".to_owned(), false)]
                );
            }
        }
    }

    fn server_plan(server: &str, project: &str, unstated: &[(&str, &str)]) -> backend::ModPlanData {
        backend::ModPlanData {
            server: true,
            instance: server.to_owned(),
            provider: "modrinth".to_owned(),
            requested: project.to_owned(),
            project: project.to_owned(),
            title: "UtilitiX".to_owned(),
            version: "21.1.6".to_owned(),
            version_id: "abcd1234".to_owned(),
            install_count: 1,
            unstated: unstated
                .iter()
                .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
                .collect(),
            ..backend::ModPlanData::default()
        }
    }

    #[test]
    fn a_server_plan_with_undeclared_support_or_a_client_only_dependency_is_confirmed() {
        assert_eq!(
            plan_disposition(true, &server_plan("server", "utilitix", &[])),
            PlanDisposition::Install
        );
        assert_eq!(
            plan_disposition(true, &server_plan("server", "utilitix", &[("lib", "Lib")])),
            PlanDisposition::Confirm
        );
        let mut client_only = server_plan("server", "utilitix", &[]);
        client_only.client_only = vec!["Lib does not run on a server.".to_owned()];
        assert_eq!(
            plan_disposition(true, &client_only),
            PlanDisposition::Confirm
        );
    }

    /// A server's Mods surface, shown on a window large enough to lay it out.
    fn server_mods_window() -> (
        AppWindow,
        Workspace,
        std::sync::mpsc::Receiver<Command>,
        InstalledHost,
    ) {
        let ui = test_window();
        register_i18n(&ui);
        ui.window().set_size(slint::PhysicalSize::new(1600, 900));
        let (workspace, commands) = test_workspace_sending(&ui);
        let server = server_host();
        workspace.state.borrow_mut().set_rows(
            Vec::new(),
            vec![ServerRow {
                id: server.id().into(),
                name: "Repro Server".into(),
                installed: true,
                loader_kind: "neoforge".into(),
                ..ServerRow::default()
            }],
        );
        workspace.open(&ui, server, Surface::Mods);
        (ui, workspace, commands, server)
    }

    /// Hands one event to the window the way the event loop does.
    fn deliver_event(ui: &AppWindow, workspace: &Workspace, event: Event) {
        apply_event(
            ui,
            event,
            &Rc::default(),
            &Rc::default(),
            &Rc::default(),
            &Rc::new(RefCell::new(MinecraftDefaultsModel::new())),
            &Rc::default(),
            &Rc::default(),
            &Rc::default(),
            &Rc::default(),
            &Rc::default(),
            workspace,
            &workspace.commands,
        );
    }

    fn render() {
        slint::platform::update_timers_and_animations();
    }

    fn on_screen(ui: &AppWindow, label: &str) -> bool {
        ElementHandle::find_by_accessible_label(ui, label).any(|element| element.size().width > 0.0)
    }

    fn server_mod(project: &str, deleted: bool) -> backend::ManagedModData {
        backend::ManagedModData {
            project_id: project.to_owned(),
            deleted,
            logical_path: format!("mods/{project}.jar"),
            title: "Repro Mod".to_owned(),
            file_name: format!("{project}.jar"),
            version: "1.0.0".to_owned(),
            version_known: true,
            provider: "Modrinth".to_owned(),
            provider_kind: "modrinth".to_owned(),
            origin: "Added".to_owned(),
            origin_kind: "user".to_owned(),
            compatibility: String::new(),
            author: String::new(),
            icon: String::new(),
            enabled: true,
            pack_owned: false,
            can_manage: true,
            can_change_version: true,
            can_update: true,
            can_remove: true,
            can_delete_file: true,
            restriction: String::new(),
            suspect_reason: String::new(),
            suspect_confidence: String::new(),
        }
    }

    /// What the backend sends whenever it lists a host's mods: the crash
    /// notice first, then the list.
    fn deliver_listing(
        ui: &AppWindow,
        workspace: &Workspace,
        server: InstalledHost,
        suggested: usize,
        mods: Vec<backend::ManagedModData>,
    ) {
        deliver_event(
            ui,
            workspace,
            Event::CrashNotice(backend::CrashNoticeData {
                host: server.id(),
                host_kind: "server".to_owned(),
                report: String::new(),
                failure: String::new(),
                client_code_on_server: false,
                suggested,
            }),
        );
        deliver_event(
            ui,
            workspace,
            Event::ManagedMods(backend::ManagedModsData {
                host: server.id(),
                host_kind: "server".to_owned(),
                mods,
                mods_are_the_users: false,
                notice: String::new(),
            }),
        );
    }

    /// A jar deleted from the list turns into its restore row at once, without
    /// leaving the surface and coming back.
    #[test]
    fn a_deleted_jar_shows_its_restore_row_as_soon_as_the_list_arrives() {
        let (ui, workspace, _commands, server) = server_mods_window();
        deliver_listing(&ui, &workspace, server, 0, vec![server_mod("repro", false)]);
        render();
        let restore = lookup("gui-restore-mod");
        assert!(!on_screen(&ui, &restore));

        deliver_listing(&ui, &workspace, server, 0, vec![server_mod("repro", true)]);
        render();

        assert!(
            on_screen(&ui, &restore),
            "the row offers to put the jar back"
        );
        assert!(
            !on_screen(
                &ui,
                &lookup_args("gui-mod-loads", [("value", Arg::from("Repro Mod"))])
            ),
            "and no longer offers to turn it off"
        );
    }

    /// A notice that really changes the host, here a crash's suggested count
    /// dropping as its suspect is put aside, does not hold back the list that
    /// follows it.
    #[test]
    fn a_list_change_shows_after_a_crash_notice_that_changed() {
        let (ui, workspace, _commands, server) = server_mods_window();
        deliver_listing(&ui, &workspace, server, 1, vec![server_mod("repro", false)]);
        render();

        deliver_listing(&ui, &workspace, server, 0, vec![server_mod("repro", true)]);
        render();

        assert!(on_screen(&ui, &lookup("gui-restore-mod")));
    }

    /// The result of a finished install says so, rather than staying queued.
    #[test]
    fn a_result_installed_on_a_server_stops_saying_it_is_queued() {
        let (ui, workspace, commands, server) = server_mods_window();
        register_browser_install(&ui, &workspace.commands, &workspace);
        register_mod_browser(&ui, &workspace.commands, &workspace);
        let id = server.id();
        ui.invoke_search_mods(
            "server".into(),
            id.as_str().into(),
            "modrinth".into(),
            "repro".into(),
        );
        let request_id = commands
            .try_iter()
            .find_map(|command| match command {
                Command::SearchMods { request_id, .. } => Some(request_id),
                _ => None,
            })
            .expect("a search");
        deliver_event(
            &ui,
            &workspace,
            Event::ModResults(backend::ModResultsData {
                host: server,
                provider: "modrinth".to_owned(),
                query: "repro".to_owned(),
                offset: 0,
                rows: vec![backend::ModSearchData {
                    project_id: "repro".to_owned(),
                    title: "Repro Mod".to_owned(),
                    description: String::new(),
                    author: String::new(),
                    icon: String::new(),
                    note: String::new(),
                    server_support: String::new(),
                    blocked_reason: String::new(),
                }],
                total: 1,
                next_offset: None,
                request_id,
                notice: String::new(),
            }),
        );
        ui.invoke_install_from_browser(
            "server".into(),
            id.as_str().into(),
            "modrinth".into(),
            "repro".into(),
        );
        apply_mod_install_plan(
            &ui,
            server_plan(&id, "repro", &[]),
            &workspace,
            &workspace.commands,
        );
        render();
        assert!(on_screen(&ui, &lookup("gui-mod-result-queued")));

        deliver_listing(&ui, &workspace, server, 0, vec![server_mod("repro", false)]);
        deliver_event(
            &ui,
            &workspace,
            Event::ModInstallFinished {
                server: true,
                instance: id.clone(),
                provider: "modrinth".to_owned(),
                project: "repro".to_owned(),
            },
        );
        render();

        assert!(on_screen(&ui, &lookup("gui-mod-result-installed")));
        assert!(!on_screen(&ui, &lookup("gui-mod-result-queued")));
    }

    /// A server's install is planned against the server and installed on it. No
    /// command the client's flow uses is issued, and the confirmation sends
    /// exactly the mods it named.
    #[test]
    fn a_server_install_issues_only_server_commands() {
        let server = ephervex_model::ServerId::new().to_string();
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_browser_install(&ui, &workspace.commands, &workspace);
        register_mod_mutations(&ui, &workspace.commands, &workspace);

        ui.invoke_install_from_browser(
            "server".into(),
            server.as_str().into(),
            "modrinth".into(),
            "utilitix".into(),
        );
        let planned: Vec<Command> = commands.try_iter().collect();
        assert!(
            matches!(planned.as_slice(), [Command::PlanServerMod { project, .. }] if project == "utilitix"),
            "{planned:?}"
        );

        // Nothing is left to decide: the plan installs.
        apply_mod_install_plan(
            &ui,
            server_plan(&server, "utilitix", &[]),
            &workspace,
            &workspace.commands,
        );
        let installed: Vec<Command> = commands.try_iter().collect();
        assert!(
            matches!(
                installed.as_slice(),
                [Command::InstallServerMod {
                    confirmed: false,
                    confirmed_support,
                    version,
                    ..
                }] if confirmed_support.is_empty() && version == "abcd1234"
            ),
            "{installed:?}"
        );
        assert_eq!(ui.get_modal(), "");
    }

    #[test]
    fn a_server_confirmation_installs_the_mods_it_named() {
        let server = ephervex_model::ServerId::new().to_string();
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_browser_install(&ui, &workspace.commands, &workspace);
        register_mod_mutations(&ui, &workspace.commands, &workspace);
        ui.invoke_install_from_browser(
            "server".into(),
            server.as_str().into(),
            "modrinth".into(),
            "utilitix".into(),
        );
        commands.try_iter().for_each(drop);

        apply_mod_install_plan(
            &ui,
            server_plan(&server, "utilitix", &[("quiet-lib", "Quiet Library")]),
            &workspace,
            &workspace.commands,
        );

        assert_eq!(ui.get_modal(), "mod-plan");
        assert_eq!(ui.get_mod_plan_kind(), "server");
        assert_eq!(ui.get_mod_plan_unstated().row_count(), 1);
        assert!(
            commands.try_iter().next().is_none(),
            "nothing installs before the confirmation"
        );

        ui.invoke_install_server_mod(
            ui.get_mod_plan_instance(),
            ui.get_mod_plan_provider(),
            ui.get_mod_plan_project(),
            ui.get_mod_plan_version_id(),
            ui.get_mod_plan_unstated_ids(),
        );
        let sent: Vec<Command> = commands.try_iter().collect();
        assert!(
            matches!(
                sent.as_slice(),
                [Command::InstallServerMod { confirmed: true, confirmed_support, .. }]
                    if confirmed_support == &["quiet-lib".to_owned()]
            ),
            "{sent:?}"
        );
    }

    /// Diagnostics names a mod, not a release, so on a server too the resolved
    /// plan is shown before anything installs, even when nothing in it needs a
    /// decision.
    #[test]
    fn a_diagnostics_install_on_a_server_is_planned_and_confirmed_before_it_installs() {
        let server = ephervex_model::ServerId::new().to_string();
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_browser_install(&ui, &workspace.commands, &workspace);
        register_mod_mutations(&ui, &workspace.commands, &workspace);

        ui.invoke_install_from_diagnostics(
            "server".into(),
            server.as_str().into(),
            "modrinth".into(),
            "utilitix".into(),
        );
        let planned: Vec<Command> = commands.try_iter().collect();
        assert!(
            matches!(planned.as_slice(), [Command::PlanServerMod { .. }]),
            "{planned:?}"
        );

        apply_mod_install_plan(
            &ui,
            server_plan(&server, "utilitix", &[]),
            &workspace,
            &workspace.commands,
        );
        assert_eq!(ui.get_modal(), "mod-plan");
        assert_eq!(ui.get_mod_plan_kind(), "server");
        assert!(
            commands.try_iter().next().is_none(),
            "nothing before confirming"
        );

        ui.invoke_install_server_mod(
            ui.get_mod_plan_instance(),
            ui.get_mod_plan_provider(),
            ui.get_mod_plan_project(),
            ui.get_mod_plan_version_id(),
            ui.get_mod_plan_unstated_ids(),
        );
        let sent: Vec<Command> = commands.try_iter().collect();
        assert!(
            matches!(
                sent.as_slice(),
                [Command::InstallServerMod {
                    confirmed: true,
                    ..
                }]
            ),
            "{sent:?}"
        );
    }

    #[test]
    fn a_queued_diagnostics_install_is_still_confirmed_when_its_turn_comes() {
        let request = |project: &str| BrowserInstall {
            server: false,
            instance: "repro-client".to_owned(),
            provider: "modrinth".to_owned(),
            project: project.to_owned(),
        };
        let mut queue = BrowserInstallQueue::default();
        assert_eq!(queue.enqueue(request("jei")), Some(request("jei")));
        assert_eq!(queue.enqueue_confirmed(request("libx")), None);
        assert!(!queue.confirms(&request("jei")));

        let next = queue.finish(&request("jei"));
        assert_eq!(next, Some(request("libx")));
        assert!(queue.confirms(&request("libx")));

        // Once it is done, the same mod from the browser installs directly.
        assert_eq!(queue.finish(&request("libx")), None);
        assert_eq!(queue.enqueue(request("libx")), Some(request("libx")));
        assert!(!queue.confirms(&request("libx")));
    }

    /// An open mod dialog, for one instance, with a linked server ticked.
    fn open_mod_dialog(ui: &AppWindow, instance: &str) {
        ui.set_mod_action_host(instance.into());
        ui.set_mod_action_path("mods/m.jar".into());
        ui.set_linked_targets(model(vec![LinkedTargetRow {
            id: ephervex_model::ServerId::new().to_string().into(),
            actionable: true,
            selected: true,
            ..LinkedTargetRow::default()
        }]));
        ui.set_modal("mod-remove".into());
    }

    fn plan_for(instance: &str, project: &str, install_count: usize) -> backend::ModPlanData {
        let mut planned = plan(install_count, 0);
        planned.instance = instance.to_owned();
        planned.linked.instance = instance.to_owned();
        planned.requested = project.to_owned();
        planned.project = project.to_owned();
        planned
    }

    /// The queue moving on to its next request must not retarget a dialog the
    /// user opened for another instance, nor clear what they ticked in it.
    #[test]
    fn the_install_queue_moving_on_leaves_an_open_dialog_alone() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_browser_install(&ui, &workspace.commands, &workspace);
        let queued = ephervex_model::InstanceId::new().to_string();
        let open = ephervex_model::InstanceId::new().to_string();
        for project in ["first", "second"] {
            ui.invoke_install_from_browser(
                "client".into(),
                queued.as_str().into(),
                "modrinth".into(),
                project.into(),
            );
        }
        open_mod_dialog(&ui, &open);
        let ticked = ui.get_linked_targets().row_data(0).expect("row").id;

        apply_mod_install_plan(
            &ui,
            plan_for(&queued, "first", 1),
            &workspace,
            &workspace.commands,
        );
        let next = workspace
            .browser
            .installs
            .borrow_mut()
            .finish(&BrowserInstall {
                server: false,
                instance: queued.clone(),
                provider: "modrinth".to_owned(),
                project: "first".to_owned(),
            });
        advance_browser_install(&workspace.commands, &workspace, next);

        assert_eq!(ui.get_mod_action_host(), open.as_str());
        assert_eq!(ui.get_modal(), "mod-remove");
        let rows = ui.get_linked_targets();
        assert_eq!(rows.row_count(), 1);
        let row = rows.row_data(0).expect("row");
        assert!(row.selected && row.id == ticked);
        let planned = commands
            .try_iter()
            .filter(|command| matches!(command, Command::PlanMod { .. }))
            .count();
        assert_eq!(planned, 2, "the second request still starts");
    }

    /// A plan that needs no confirmation installs whatever dialog is open, and
    /// never takes the servers ticked in that dialog.
    #[test]
    fn a_direct_install_goes_ahead_under_an_open_dialog_without_its_servers() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_browser_install(&ui, &workspace.commands, &workspace);
        let queued = ephervex_model::InstanceId::new().to_string();
        ui.invoke_install_from_browser(
            "client".into(),
            queued.as_str().into(),
            "modrinth".into(),
            "first".into(),
        );
        open_mod_dialog(&ui, &queued);

        apply_mod_install_plan(
            &ui,
            plan_for(&queued, "first", 1),
            &workspace,
            &workspace.commands,
        );

        let installs: Vec<_> = commands
            .try_iter()
            .filter_map(|command| match command {
                Command::InstallMod {
                    project,
                    servers,
                    confirmed,
                    ..
                } => Some((project, servers, confirmed)),
                _ => None,
            })
            .collect();
        // Unconfirmed, so finishing it has no dialog of its own to close.
        assert_eq!(installs, [("first".to_owned(), Vec::new(), false)]);
        assert_eq!(ui.get_modal(), "mod-remove");
        assert_eq!(ui.get_linked_targets().row_count(), 1);
    }

    /// A confirmation cannot replace the dialog the user is in. The request
    /// ends rather than waiting forever, and the next one still starts.
    #[test]
    fn a_confirmation_that_cannot_be_shown_ends_its_request() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_browser_install(&ui, &workspace.commands, &workspace);
        let queued = ephervex_model::InstanceId::new().to_string();
        for project in ["first", "second"] {
            ui.invoke_install_from_diagnostics(
                "client".into(),
                queued.as_str().into(),
                "modrinth".into(),
                project.into(),
            );
        }
        open_mod_dialog(&ui, &queued);
        commands.try_iter().for_each(drop);

        apply_mod_install_plan(
            &ui,
            plan_for(&queued, "first", 1),
            &workspace,
            &workspace.commands,
        );

        assert_eq!(ui.get_modal(), "mod-remove");
        let active = workspace.browser.installs.borrow().active.clone();
        assert_eq!(active.map(|request| request.project), Some("second".into()));
        assert!(commands.try_iter().any(
            |command| matches!(command, Command::PlanMod { project, .. } if project == "second")
        ));
    }

    /// The answer to a mod dialog the user already closed cannot replace the
    /// install confirmation that opened afterwards, which would leave the queue
    /// waiting on a confirmation nobody can answer.
    #[test]
    fn a_late_answer_for_a_closed_dialog_cannot_replace_an_install_confirmation() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_browser_install(&ui, &workspace.commands, &workspace);
        let queued = ephervex_model::InstanceId::new().to_string();
        let closed = ephervex_model::InstanceId::new().to_string();
        let request = ask_from_manager(&ui, &workspace, &commands, &closed, PlanKind::Apply);
        ui.set_modal(SharedString::default());
        ui.invoke_install_from_diagnostics(
            "client".into(),
            queued.as_str().into(),
            "modrinth".into(),
            "first".into(),
        );
        apply_mod_install_plan(
            &ui,
            plan_for(&queued, "first", 1),
            &workspace,
            &workspace.commands,
        );
        assert_eq!(ui.get_modal(), "mod-plan");

        let mut late = plan_for(&closed, "first", 1).linked;
        late.path = "mods/m.jar".to_owned();
        apply_linked_confirmation(&ui, request, late, &workspace);

        assert_eq!(ui.get_modal(), "mod-plan");
        assert_eq!(ui.get_mod_plan_instance(), queued.as_str());
        assert!(workspace.browser.installs.borrow().active.is_some());
    }

    /// Opens the Manage dialog for a mod and asks for the plan `kind` names, as
    /// its buttons do, returning the number the plan will answer.
    fn ask_from_manager(
        ui: &AppWindow,
        workspace: &Workspace,
        commands: &std::sync::mpsc::Receiver<Command>,
        instance: &str,
        kind: PlanKind,
    ) -> u64 {
        register_mod_actions(ui, &workspace.commands, workspace);
        ui.invoke_open_mod_manager("client".into(), instance.into(), "mods/m.jar".into());
        commands.try_iter().for_each(drop);
        match kind {
            PlanKind::Removal => {
                ui.invoke_plan_mod_removal("client".into(), instance.into(), "mods/m.jar".into());
            }
            PlanKind::Apply => {
                ui.invoke_plan_mod_apply(
                    instance.into(),
                    "mods/m.jar".into(),
                    String::new().into(),
                );
            }
        }
        requested_plan(commands)
    }

    /// A server's mod manager reads, changes, and removes the server's own
    /// mod, never a client's.
    #[test]
    fn a_servers_mod_manager_addresses_every_request_to_the_server() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_mod_actions(&ui, &workspace.commands, &workspace);
        let server = ephervex_model::ServerId::new();
        let host = InstalledHost::Server(server);
        let id = SharedString::from(server.to_string());

        ui.invoke_open_mod_manager("server".into(), id.clone(), "mods/m.jar".into());
        let opened: Vec<Command> = commands.try_iter().collect();
        assert!(
            opened.iter().any(
                |command| matches!(command, Command::LoadModVersions { host: asked, .. } if *asked == host)
            ),
            "{opened:?}"
        );

        ui.invoke_plan_mod_apply(id.clone(), "mods/m.jar".into(), "v2".into());
        ui.invoke_change_mod_version(id.clone(), "mods/m.jar".into(), "v2".into());
        ui.invoke_plan_mod_removal("server".into(), id.clone(), "mods/m.jar".into());
        ui.invoke_remove_mod(id, "mods/m.jar".into());
        let hosts: Vec<InstalledHost> = commands
            .try_iter()
            .filter_map(|command| match command {
                Command::PlanModApply { host, .. }
                | Command::ChangeModVersion { host, .. }
                | Command::PlanModRemoval { host, .. }
                | Command::RemoveMod { host, .. } => Some(host),
                _ => None,
            })
            .collect();
        assert_eq!(hosts, [host; 4]);
    }

    /// The number the next plan command carries.
    fn requested_plan(commands: &std::sync::mpsc::Receiver<Command>) -> u64 {
        commands
            .try_iter()
            .find_map(|command| match command {
                Command::PlanModRemoval { request, .. } | Command::PlanModApply { request, .. } => {
                    Some(request)
                }
                _ => None,
            })
            .expect("a plan was asked for")
    }

    fn removal_plan(request: u64, instance: &str) -> backend::ModRemovalPlanData {
        backend::ModRemovalPlanData {
            request,
            instance: instance.to_owned(),
            logical_path: "mods/m.jar".to_owned(),
            name: "UtilitiX".to_owned(),
            ..backend::ModRemovalPlanData::default()
        }
    }

    #[test]
    fn a_removal_plan_is_shown_once_and_a_repeat_does_not_reopen_the_dialog() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        let host = ephervex_model::InstanceId::new().to_string();
        let request = ask_from_manager(&ui, &workspace, &commands, &host, PlanKind::Removal);

        apply_mod_removal_plan(&ui, removal_plan(request, &host), &workspace);
        assert_eq!(ui.get_modal(), "mod-remove");
        assert!(ui.get_mod_remove_returns_to_manager());

        // The removal was confirmed and the dialog is gone.
        ui.set_modal(SharedString::default());
        apply_mod_removal_plan(&ui, removal_plan(request, &host), &workspace);

        assert_eq!(ui.get_modal(), "");
    }

    #[test]
    fn only_the_newest_removal_ask_is_answered() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        let host = ephervex_model::InstanceId::new().to_string();
        let first = ask_from_manager(&ui, &workspace, &commands, &host, PlanKind::Removal);
        ui.invoke_plan_mod_removal("client".into(), host.as_str().into(), "mods/m.jar".into());
        let second = requested_plan(&commands);

        apply_mod_removal_plan(&ui, removal_plan(first, &host), &workspace);
        assert_eq!(ui.get_modal(), "mod-manage");
        apply_mod_removal_plan(&ui, removal_plan(second, &host), &workspace);
        assert_eq!(ui.get_modal(), "mod-remove");
    }

    #[test]
    fn a_plan_for_a_manage_dialog_the_user_closed_is_not_shown() {
        let ui = test_window();
        let toasts = toasts(&ui);
        let (workspace, commands) = test_workspace_sending(&ui);
        let host = ephervex_model::InstanceId::new().to_string();
        for kind in [PlanKind::Removal, PlanKind::Apply] {
            let request = ask_from_manager(&ui, &workspace, &commands, &host, kind);
            ui.set_modal(SharedString::default());

            match kind {
                PlanKind::Removal => {
                    apply_mod_removal_plan(&ui, removal_plan(request, &host), &workspace);
                }
                PlanKind::Apply => apply_linked_confirmation(
                    &ui,
                    request,
                    backend::LinkedSectionData::default(),
                    &workspace,
                ),
            }

            assert_eq!(ui.get_modal(), "", "{kind:?} reopened a closed dialog");
        }
        assert!(
            toasts.borrow().is_empty(),
            "leaving a dialog is not an error"
        );
    }

    #[test]
    fn a_plan_for_an_earlier_opening_of_the_manage_dialog_is_not_shown() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        let host = ephervex_model::InstanceId::new().to_string();
        let request = ask_from_manager(&ui, &workspace, &commands, &host, PlanKind::Apply);

        ui.set_modal(SharedString::default());
        ui.invoke_open_mod_manager("client".into(), host.as_str().into(), "mods/m.jar".into());
        apply_linked_confirmation(
            &ui,
            request,
            backend::LinkedSectionData::default(),
            &workspace,
        );

        assert_eq!(ui.get_modal(), "mod-manage");
    }

    #[test]
    fn a_confirmation_follows_the_manage_dialog_that_asked_for_it() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        let host = ephervex_model::InstanceId::new().to_string();
        let request = ask_from_manager(&ui, &workspace, &commands, &host, PlanKind::Apply);

        apply_linked_confirmation(
            &ui,
            request,
            backend::LinkedSectionData::default(),
            &workspace,
        );

        assert_eq!(ui.get_modal(), "mod-apply");
    }

    #[test]
    fn a_card_removal_plan_that_finds_an_install_confirmation_open_says_it_was_not_shown() {
        let ui = test_window();
        let toasts = toasts(&ui);
        let (workspace, commands) = test_workspace_sending(&ui);
        register_mod_mutations(&ui, &workspace.commands, &workspace);
        let host = ephervex_model::InstanceId::new().to_string();
        ui.invoke_plan_mod_removal("client".into(), host.as_str().into(), "mods/m.jar".into());
        let request = requested_plan(&commands);
        open_install_confirmation(&ui, &workspace);

        apply_mod_removal_plan(&ui, removal_plan(request, &host), &workspace);

        assert_eq!(ui.get_modal(), "mod-plan");
        assert!(workspace.browser.installs.borrow().active.is_some());
        assert_eq!(
            toasts.borrow().as_slice(),
            [(lookup("gui-dialog-not-shown-open-dialog"), true)]
        );
    }

    #[test]
    fn a_card_removal_plan_is_shown_for_its_own_mod_after_an_install_confirmation_took_the_addressing()
     {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_mod_mutations(&ui, &workspace.commands, &workspace);
        let host = ephervex_model::InstanceId::new().to_string();
        let other = ephervex_model::InstanceId::new().to_string();
        ui.set_mod_action_path("mods/m.jar".into());
        ui.invoke_plan_mod_removal("client".into(), host.as_str().into(), "mods/m.jar".into());
        let request = requested_plan(&commands);
        // An install confirmation addressed its own instance, then closed.
        ui.set_mod_action_host(other.as_str().into());
        ui.set_mod_action_path(SharedString::default());

        apply_mod_removal_plan(&ui, removal_plan(request, &host), &workspace);

        assert_eq!(ui.get_modal(), "mod-remove");
        assert_eq!(ui.get_mod_action_host(), host.as_str());
        assert_eq!(ui.get_mod_action_path(), "mods/m.jar");
        assert!(!ui.get_mod_remove_returns_to_manager());
    }

    #[test]
    fn a_plan_nothing_is_waiting_for_is_ignored() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_browser_install(&ui, &workspace.commands, &workspace);
        let instance = ephervex_model::InstanceId::new().to_string();

        apply_mod_install_plan(
            &ui,
            plan_for(&instance, "first", 1),
            &workspace,
            &workspace.commands,
        );

        assert_eq!(ui.get_modal(), "");
        assert_eq!(commands.try_iter().count(), 0);
    }

    fn acquisition_prompt(id: u64, instance: &str) -> backend::ModAcquisitionData {
        backend::ModAcquisitionData {
            id,
            instance: instance.to_owned(),
            logical_path: String::new(),
            name: "UtilitiX".to_owned(),
            items: Vec::new(),
        }
    }

    /// A release that must be fetched by hand is asked for whichever dialog
    /// the window showed before, because the wait has no other way to be seen.
    #[test]
    fn a_manual_download_prompt_is_shown_without_any_mod_dialog_open() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        let server = ephervex_model::ServerId::new().to_string();

        apply_mod_acquisition(&ui, acquisition_prompt(1, &server), &workspace);

        assert_eq!(ui.get_modal(), "mod-acquire");
        assert_eq!(ui.get_mod_acquisition_id(), "1");
    }

    #[test]
    fn a_manual_download_prompt_over_an_install_confirmation_releases_the_install_queue() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        register_browser_install(&ui, &workspace.commands, &workspace);
        let queued = ephervex_model::InstanceId::new().to_string();
        ui.invoke_install_from_browser(
            "client".into(),
            queued.as_str().into(),
            "modrinth".into(),
            "first".into(),
        );
        ui.set_modal("mod-plan".into());
        assert!(workspace.browser.installs.borrow().active.is_some());
        let other = ephervex_model::InstanceId::new().to_string();

        apply_mod_acquisition(&ui, acquisition_prompt(1, &other), &workspace);
        assert_eq!(ui.get_modal(), "mod-acquire");
        end_mod_acquisition(&ui, 1, &workspace);

        assert!(
            workspace.browser.installs.borrow().active.is_none(),
            "the replaced confirmation no longer holds the queue's running request"
        );
    }

    #[test]
    fn a_second_manual_download_prompt_waits_behind_the_one_on_screen() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        let host = ephervex_model::InstanceId::new().to_string();
        apply_mod_acquisition(&ui, acquisition_prompt(1, &host), &workspace);

        apply_mod_acquisition(&ui, acquisition_prompt(2, &host), &workspace);
        assert_eq!(
            ui.get_mod_acquisition_id(),
            "1",
            "the prompt on screen keeps the cancel button its wait needs"
        );

        end_mod_acquisition(&ui, 1, &workspace);
        assert_eq!(ui.get_modal(), "mod-acquire");
        assert_eq!(ui.get_mod_acquisition_id(), "2");

        end_mod_acquisition(&ui, 2, &workspace);
        assert_eq!(ui.get_modal(), "");
    }

    fn stand_in_prompt(id: u64) -> backend::ModStandInData {
        backend::ModStandInData {
            id,
            detail: "mods/libx-dev.jar already provides LibX, which this mod needs.".to_owned(),
            note: String::new(),
        }
    }

    #[test]
    fn a_stand_in_question_waits_behind_a_manual_download_prompt_and_follows_it() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        let host = ephervex_model::InstanceId::new().to_string();
        apply_mod_acquisition(&ui, acquisition_prompt(1, &host), &workspace);

        apply_mod_stand_in(&ui, stand_in_prompt(2), &workspace);
        assert_eq!(ui.get_modal(), "mod-acquire");

        end_mod_acquisition(&ui, 1, &workspace);
        assert_eq!(ui.get_modal(), "mod-stand-in");
        assert_eq!(ui.get_mod_stand_in_id(), "2");
        assert!(ui.get_mod_stand_in_detail().contains("libx-dev.jar"));
    }

    #[test]
    fn a_manual_download_prompt_waits_behind_a_stand_in_question() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        let host = ephervex_model::InstanceId::new().to_string();
        apply_mod_stand_in(&ui, stand_in_prompt(1), &workspace);

        apply_mod_acquisition(&ui, acquisition_prompt(2, &host), &workspace);
        assert_eq!(
            ui.get_modal(),
            "mod-stand-in",
            "the question on screen keeps the buttons its operation waits on"
        );

        end_mod_stand_in(&ui, 1, &workspace);
        assert_eq!(ui.get_modal(), "mod-acquire");
        assert_eq!(ui.get_mod_acquisition_id(), "2");
    }

    #[test]
    fn answering_a_stand_in_question_sends_the_answer_and_closes_it() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_stand_in_answers(&ui, &workspace.commands, &workspace);
        apply_mod_stand_in(&ui, stand_in_prompt(7), &workspace);
        apply_mod_stand_in(&ui, stand_in_prompt(8), &workspace);

        ui.invoke_answer_mod_stand_in("7".into(), true);

        assert!(
            commands.try_iter().any(|command| matches!(
                command,
                Command::AnswerModStandIn {
                    id: 7,
                    accepted: true
                }
            )),
            "the answer reaches the operation"
        );
        assert_eq!(ui.get_modal(), "mod-stand-in", "the next question follows");
        assert_eq!(ui.get_mod_stand_in_id(), "8");

        end_mod_stand_in(&ui, 7, &workspace);
        assert_eq!(
            ui.get_mod_stand_in_id(),
            "8",
            "a late end leaves the next question"
        );
    }

    #[test]
    fn a_queued_manual_download_prompt_that_ends_is_never_shown() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        let host = ephervex_model::InstanceId::new().to_string();
        apply_mod_acquisition(&ui, acquisition_prompt(1, &host), &workspace);
        apply_mod_acquisition(&ui, acquisition_prompt(2, &host), &workspace);

        end_mod_acquisition(&ui, 2, &workspace);
        end_mod_acquisition(&ui, 1, &workspace);

        assert_eq!(ui.get_modal(), "");
    }

    #[test]
    fn a_finished_manual_download_wait_leaves_an_unrelated_dialog_open() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        let host = ephervex_model::InstanceId::new().to_string();
        apply_mod_acquisition(&ui, acquisition_prompt(1, &host), &workspace);
        ui.set_modal("rename".into());

        end_mod_acquisition(&ui, 1, &workspace);

        assert_eq!(ui.get_modal(), "rename");
    }

    /// Opens the install confirmation the browser's queue is waiting on.
    fn open_install_confirmation(ui: &AppWindow, workspace: &Workspace) {
        register_browser_install(ui, &workspace.commands, workspace);
        let queued = ephervex_model::InstanceId::new().to_string();
        ui.invoke_install_from_browser(
            "client".into(),
            queued.as_str().into(),
            "modrinth".into(),
            "first".into(),
        );
        ui.set_modal("mod-plan".into());
        assert!(workspace.browser.installs.borrow().active.is_some());
    }

    fn linked_result(scope: backend::ModalScope, tone: &str) -> backend::LinkedResultData {
        backend::LinkedResultData {
            scope,
            title: "UtilitiX".to_owned(),
            summary: "Applied to every server.".to_owned(),
            rows: vec![backend::LinkedTargetData {
                name: "Server".to_owned(),
                tone: tone.to_owned(),
                ..backend::LinkedTargetData::default()
            }],
        }
    }

    #[test]
    fn another_operations_completion_leaves_an_install_confirmation_open() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        open_install_confirmation(&ui, &workspace);

        close_modal_for(&ui, backend::ModalScope::Dialogs(&["snapshot-create"]));

        assert_eq!(ui.get_modal(), "mod-plan");
        assert!(workspace.browser.installs.borrow().active.is_some());
    }

    #[test]
    fn a_completion_closes_the_dialog_it_owns_and_clears_its_pending_state() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        let _ = workspace;
        ui.set_modal("rename".into());
        ui.set_modal_error("Name taken".into());
        ui.set_mod_action_pending(true);

        close_modal_for(&ui, backend::ModalScope::RENAME);

        assert_eq!(ui.get_modal(), "");
        assert_eq!(ui.get_modal_error(), "");
        assert!(!ui.get_mod_action_pending());
    }

    #[test]
    fn a_completion_whose_dialog_already_closed_still_releases_the_pending_state() {
        let ui = test_window();
        ui.set_mod_action_pending(true);

        close_modal_for(&ui, backend::ModalScope::NONE);

        assert_eq!(ui.get_modal(), "");
        assert!(!ui.get_mod_action_pending());
    }

    #[test]
    fn a_completion_that_closes_an_install_confirmation_releases_the_install_queue() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        open_install_confirmation(&ui, &workspace);

        close_modal_for(&ui, backend::ModalScope::Dialogs(&["mod-plan"]));

        assert_eq!(ui.get_modal(), "");
        assert!(workspace.browser.installs.borrow().active.is_none());
    }

    #[test]
    fn another_operations_completion_leaves_a_manual_download_prompt_and_its_queue() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        let host = ephervex_model::InstanceId::new().to_string();
        apply_mod_acquisition(&ui, acquisition_prompt(1, &host), &workspace);
        apply_mod_acquisition(&ui, acquisition_prompt(2, &host), &workspace);

        close_modal_for(&ui, backend::ModalScope::MOD_MANAGE);
        apply_linked_result(&ui, linked_result(backend::ModalScope::NONE, "error"));

        assert_eq!(ui.get_modal(), "mod-acquire");
        assert_eq!(ui.get_mod_acquisition_id(), "1");
        end_mod_acquisition(&ui, 1, &workspace);
        assert_eq!(
            ui.get_mod_acquisition_id(),
            "2",
            "the queued prompt is still there to be shown"
        );
    }

    #[test]
    fn a_linked_report_is_a_toast_when_another_dialog_is_open() {
        let ui = test_window();
        let toasts = toasts(&ui);
        ui.set_modal("rename".into());

        apply_linked_result(&ui, linked_result(backend::ModalScope::NONE, "error"));

        assert_eq!(ui.get_modal(), "rename");
        assert_eq!(
            toasts.borrow().as_slice(),
            [("Applied to every server.".to_owned(), true)]
        );
    }

    #[test]
    fn an_unowned_linked_report_leaves_an_open_report_as_it_was() {
        let ui = test_window();
        apply_linked_result(&ui, linked_result(backend::ModalScope::NONE, "error"));
        assert_eq!(ui.get_modal(), "mod-result");
        let mut other = linked_result(backend::ModalScope::NONE, "error");
        other.title = "Another mod".to_owned();
        other.rows.clear();

        apply_linked_result(&ui, other);

        assert_eq!(ui.get_linked_result_title(), "UtilitiX");
        assert_eq!(ui.get_linked_results().row_count(), 1);
    }

    #[test]
    fn a_linked_report_opens_when_no_dialog_is_in_the_way() {
        let ui = test_window();

        apply_linked_result(&ui, linked_result(backend::ModalScope::NONE, "error"));

        assert_eq!(ui.get_modal(), "mod-result");
    }

    #[test]
    fn a_linked_report_replacing_an_install_confirmation_releases_the_install_queue() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        open_install_confirmation(&ui, &workspace);

        apply_linked_result(
            &ui,
            linked_result(backend::ModalScope::Dialogs(&["mod-plan"]), "error"),
        );

        assert_eq!(ui.get_modal(), "mod-result");
        assert!(workspace.browser.installs.borrow().active.is_none());
    }

    #[test]
    fn a_failure_goes_to_the_dialog_it_is_about_while_that_is_open() {
        let ui = test_window();
        let toasts = toasts(&ui);
        ui.set_modal("mod-manage".into());
        ui.set_mod_action_pending(true);

        apply_modal_error(
            &ui,
            backend::ModalScope::MOD_MANAGE,
            "Download failed".into(),
        );

        assert_eq!(ui.get_modal_error(), "Download failed");
        assert!(!ui.get_mod_action_pending());
        assert!(toasts.borrow().is_empty());
    }

    #[test]
    fn a_failure_after_its_dialog_closed_is_a_toast_and_leaves_no_stray_error() {
        let ui = test_window();
        let toasts = toasts(&ui);

        apply_modal_error(&ui, backend::ModalScope::NONE, "Download failed".into());
        ui.set_modal("rename".into());
        apply_modal_error(&ui, backend::ModalScope::NONE, "Download failed".into());

        assert_eq!(ui.get_modal_error(), "");
        assert_eq!(
            toasts.borrow().as_slice(),
            [
                ("Download failed".to_owned(), true),
                ("Download failed".to_owned(), true)
            ]
        );
    }

    #[test]
    fn a_failure_that_is_not_about_a_dialog_still_shows_where_forms_read_it() {
        let ui = test_window();
        let toasts = toasts(&ui);

        apply_modal_error(&ui, backend::ModalScope::Anywhere, "Not saved".into());

        assert_eq!(ui.get_modal_error(), "Not saved");
        assert!(toasts.borrow().is_empty());
    }

    /// A window whose host views hold the given clients, with the loader-change
    /// dialog's action registered.
    fn loader_change_window(
        clients: &[(&str, &str, &str)],
    ) -> (AppWindow, std::sync::mpsc::Receiver<Command>) {
        let ui = test_window();
        ui.set_host_views(model(
            clients
                .iter()
                .map(|(id, loader_kind, minecraft)| HostView {
                    kind: "client".into(),
                    id: (*id).into(),
                    client: InstanceRow {
                        loader_kind: (*loader_kind).into(),
                        minecraft: (*minecraft).into(),
                        loader: format!("{loader_kind} 21.1.1").into(),
                        ..InstanceRow::default()
                    },
                    ..HostView::default()
                })
                .collect(),
        ));
        let (sender, commands) = std::sync::mpsc::channel();
        register_loader_change(&ui, &sender);
        (ui, commands)
    }

    fn loader_builds(builds: &[&str]) -> Vec<String> {
        builds.iter().map(|build| (*build).to_owned()).collect()
    }

    fn new_instance() -> ephervex_model::InstanceId {
        ephervex_model::InstanceId::new()
    }

    fn open_loader_change(ui: &AppWindow, instance: ephervex_model::InstanceId) {
        ui.invoke_open_loader_change(instance.to_string().into());
    }

    #[test]
    fn a_refused_loader_change_leaves_the_change_button_usable() {
        let a = new_instance();
        let (ui, _commands) = loader_change_window(&[(&a.to_string(), "NeoForge", "1.21.1")]);
        open_loader_change(&ui, a);
        ui.set_loader_change_pending(true);

        apply_modal_error(
            &ui,
            backend::ModalScope::LOADER_CHANGE,
            "Not compatible".into(),
        );

        assert_eq!(ui.get_modal_error(), "Not compatible");
        assert!(!ui.get_loader_change_pending());
    }

    #[test]
    fn a_failure_for_another_dialog_does_not_release_a_loader_change_in_flight() {
        let a = new_instance();
        let (ui, _commands) = loader_change_window(&[(&a.to_string(), "NeoForge", "1.21.1")]);
        open_loader_change(&ui, a);
        ui.set_loader_change_pending(true);

        apply_modal_error(&ui, backend::ModalScope::Anywhere, "Unrelated".into());
        apply_modal_error(&ui, backend::ModalScope::RENAME, "Unrelated".into());

        assert!(ui.get_loader_change_pending());
    }

    #[test]
    fn a_loader_change_failure_after_its_dialog_closed_is_a_toast_beside_the_open_dialog() {
        let a = new_instance();
        let (ui, _commands) = loader_change_window(&[]);
        let toasts = toasts(&ui);
        ui.set_modal("rename".into());

        apply_loader_change_failure(&ui, a, "Not compatible".into());

        assert_eq!(ui.get_modal_error(), "");
        assert_eq!(
            toasts.borrow().as_slice(),
            [("Not compatible".to_owned(), true)]
        );
    }

    #[test]
    fn a_version_list_for_another_loader_or_release_is_not_applied_to_the_open_loader_change() {
        let b = new_instance();
        let (ui, _commands) = loader_change_window(&[(&b.to_string(), "NeoForge", "1.21.1")]);
        open_loader_change(&ui, b);

        apply_loader_versions(
            &ui,
            "loader-change",
            ephervex_model::LoaderKind::Forge,
            "1.21.1",
            &loader_builds(&["52.0.1"]),
        );
        apply_loader_versions(
            &ui,
            "loader-change",
            ephervex_model::LoaderKind::NeoForge,
            "1.20.1",
            &loader_builds(&["47.1.0"]),
        );

        assert!(ui.get_loader_change_loading());
        assert_eq!(ui.get_loader_change_versions().row_count(), 0);
        assert_eq!(ui.get_loader_change_selected(), "");
    }

    #[test]
    fn a_version_list_for_the_open_loader_change_fills_it_and_defaults_to_the_current_build() {
        let b = new_instance();
        let (ui, _commands) = loader_change_window(&[(&b.to_string(), "NeoForge", "1.21.1")]);
        open_loader_change(&ui, b);

        apply_loader_versions(
            &ui,
            "loader-change",
            ephervex_model::LoaderKind::NeoForge,
            "1.21.1",
            &loader_builds(&["21.1.2", "21.1.1"]),
        );

        assert!(!ui.get_loader_change_loading());
        assert_eq!(ui.get_loader_change_versions().row_count(), 2);
        assert_eq!(ui.get_loader_change_selected(), "21.1.1");
    }

    #[test]
    fn a_loader_change_list_without_the_running_build_chooses_nothing() {
        let b = new_instance();
        let (ui, _commands) = loader_change_window(&[(&b.to_string(), "NeoForge", "1.21.1")]);
        open_loader_change(&ui, b);

        apply_loader_versions(
            &ui,
            "loader-change",
            ephervex_model::LoaderKind::NeoForge,
            "1.21.1",
            &loader_builds(&["21.1.3", "21.1.2"]),
        );

        assert_eq!(ui.get_loader_change_current(), "NeoForge 21.1.1");
        assert_eq!(ui.get_loader_change_current_build(), "21.1.1");
        assert_eq!(ui.get_loader_change_selected(), "");
    }

    #[test]
    fn a_version_list_arriving_after_the_loader_change_dialog_closed_is_dropped() {
        let a = new_instance();
        let (ui, _commands) = loader_change_window(&[(&a.to_string(), "NeoForge", "1.21.1")]);
        open_loader_change(&ui, a);
        ui.set_modal(SharedString::default());

        apply_loader_versions(
            &ui,
            "loader-change",
            ephervex_model::LoaderKind::NeoForge,
            "1.21.1",
            &loader_builds(&["21.1.2"]),
        );

        assert!(ui.get_loader_change_loading());
        assert_eq!(ui.get_loader_change_versions().row_count(), 0);
    }

    #[test]
    fn opening_the_loader_change_dialog_asks_for_builds_on_behalf_of_its_instance() {
        let a = new_instance();
        let (ui, commands) = loader_change_window(&[(&a.to_string(), "NeoForge", "1.21.1")]);

        open_loader_change(&ui, a);

        match commands.try_recv() {
            Ok(Command::LoadLoaderVersions {
                form,
                instance,
                loader,
                minecraft,
            }) => {
                assert_eq!(form, "loader-change");
                assert_eq!(instance, Some(a));
                assert_eq!(loader, ephervex_model::LoaderKind::NeoForge);
                assert_eq!(minecraft, "1.21.1");
            }
            other => panic!("expected a build list request, got {other:?}"),
        }
    }

    #[test]
    fn a_failed_build_list_ends_the_loading_of_the_dialog_it_was_asked_for() {
        let a = new_instance();
        let (ui, _commands) = loader_change_window(&[(&a.to_string(), "NeoForge", "1.21.1")]);
        let toasts = toasts(&ui);
        open_loader_change(&ui, a);
        assert!(ui.get_loader_change_loading());

        apply_loader_versions_failure(
            &ui,
            a,
            ephervex_model::LoaderKind::NeoForge,
            "1.21.1",
            "Offline".into(),
        );

        assert!(!ui.get_loader_change_loading());
        assert_eq!(ui.get_modal_error(), "Offline");
        assert!(toasts.borrow().is_empty());
    }

    #[test]
    fn a_failed_build_list_for_a_dialog_since_reopened_for_another_instance_is_a_toast() {
        let (a, b) = (new_instance(), new_instance());
        let (ui, _commands) = loader_change_window(&[
            (&a.to_string(), "NeoForge", "1.21.1"),
            (&b.to_string(), "NeoForge", "1.21.1"),
        ]);
        let toasts = toasts(&ui);
        open_loader_change(&ui, a);
        open_loader_change(&ui, b);

        apply_loader_versions_failure(
            &ui,
            a,
            ephervex_model::LoaderKind::NeoForge,
            "1.21.1",
            "Offline".into(),
        );

        assert!(ui.get_loader_change_loading());
        assert_eq!(ui.get_modal_error(), "");
        assert_eq!(toasts.borrow().as_slice(), [("Offline".to_owned(), true)]);
    }

    #[test]
    fn a_failed_build_list_after_the_loader_change_dialog_closed_is_a_toast() {
        let a = new_instance();
        let (ui, _commands) = loader_change_window(&[(&a.to_string(), "NeoForge", "1.21.1")]);
        let toasts = toasts(&ui);
        open_loader_change(&ui, a);
        ui.set_modal(SharedString::default());

        apply_loader_versions_failure(
            &ui,
            a,
            ephervex_model::LoaderKind::NeoForge,
            "1.21.1",
            "Offline".into(),
        );

        assert_eq!(ui.get_modal_error(), "");
        assert_eq!(toasts.borrow().as_slice(), [("Offline".to_owned(), true)]);
    }

    #[test]
    fn a_loader_change_failure_for_another_instance_leaves_the_open_dialog_alone() {
        let (a, b) = (new_instance(), new_instance());
        let (ui, _commands) = loader_change_window(&[
            (&a.to_string(), "NeoForge", "1.21.1"),
            (&b.to_string(), "NeoForge", "1.21.1"),
        ]);
        let toasts = toasts(&ui);
        open_loader_change(&ui, a);
        open_loader_change(&ui, b);
        ui.set_loader_change_pending(true);

        apply_loader_change_failure(&ui, a, "Not compatible".into());

        assert_eq!(ui.get_modal(), "loader-change");
        assert_eq!(ui.get_modal_error(), "");
        assert!(ui.get_loader_change_pending());
        assert_eq!(
            toasts.borrow().as_slice(),
            [("Not compatible".to_owned(), true)]
        );
    }

    #[test]
    fn a_loader_change_failure_for_the_open_instance_is_shown_in_its_dialog() {
        let a = new_instance();
        let (ui, _commands) = loader_change_window(&[(&a.to_string(), "NeoForge", "1.21.1")]);
        let toasts = toasts(&ui);
        open_loader_change(&ui, a);
        ui.set_loader_change_pending(true);

        apply_loader_change_failure(&ui, a, "Not compatible".into());

        assert_eq!(ui.get_modal_error(), "Not compatible");
        assert!(!ui.get_loader_change_pending());
        assert!(toasts.borrow().is_empty());
    }

    #[test]
    fn a_finished_loader_change_closes_only_the_dialog_open_for_its_instance() {
        let (a, b) = (new_instance(), new_instance());
        let (ui, _commands) = loader_change_window(&[
            (&a.to_string(), "NeoForge", "1.21.1"),
            (&b.to_string(), "NeoForge", "1.21.1"),
        ]);
        open_loader_change(&ui, a);
        open_loader_change(&ui, b);

        apply_loader_change_finished(&ui, a);
        assert_eq!(ui.get_modal(), "loader-change");

        apply_loader_change_finished(&ui, b);
        assert_eq!(ui.get_modal(), "");
    }

    /// A link request the backend has answered with its download, as the window
    /// sees it. Returns the request's number.
    fn pack_link_downloading(
        ui: &AppWindow,
        commands: &Sender<Command>,
        import_flow: &Rc<RefCell<ImportFlowState>>,
    ) -> i32 {
        let generation = begin_pack_link_request(ui);
        apply_pack_link_event(
            ui,
            Event::PackUrlDownloading {
                generation,
                url: "https://packs.example/pack.mrpack".to_owned(),
                display: "https://packs.example/pack.mrpack".to_owned(),
                host: "packs.example".to_owned(),
            },
            import_flow,
            commands,
        );
        generation
    }

    fn pack_link_cancel_wiring(ui: &AppWindow, commands: &Sender<Command>) {
        let weak = ui.as_weak();
        let sender = commands.clone();
        ui.on_cancel_pack_link(move || {
            if let Some(ui) = weak.upgrade() {
                cancel_pack_link_download(&ui, &sender);
            }
        });
    }

    fn archive_in(base: &tempfile::TempDir, name: &str) -> PathBuf {
        let archive = base.path().join(name);
        std::fs::write(&archive, b"a pack").expect("write");
        archive
    }

    #[test]
    fn a_link_download_is_shown_and_canceling_it_stops_it_and_opens_no_import() {
        let ui = test_window();
        let (commands, received) = std::sync::mpsc::channel();
        let import_flow = Rc::new(RefCell::new(ImportFlowState::default()));
        pack_link_cancel_wiring(&ui, &commands);
        let base = tempfile::tempdir().expect("a disposable root");
        let archive = archive_in(&base, "link.pack");

        let generation = pack_link_downloading(&ui, &commands, &import_flow);
        assert_eq!(ui.get_modal(), "open-pack-link");
        assert!(ui.get_pack_link_downloading());

        ui.invoke_cancel_pack_link();
        assert!(!ui.get_pack_link_downloading());
        assert!(matches!(
            received.try_recv(),
            Ok(Command::CancelPackUrl { generation: stopped }) if stopped == generation
        ));

        // The archive was already on its way when Cancel was pressed.
        ui.set_modal(SharedString::default());
        apply_pack_link_event(
            &ui,
            Event::PackDownloaded {
                generation,
                archive: archive.clone(),
            },
            &import_flow,
            &commands,
        );

        assert_eq!(
            ui.get_modal(),
            "",
            "no import opens over what the user is doing"
        );
        assert!(received.try_recv().is_err(), "nothing analyzes the archive");
        assert!(!archive.exists(), "the unwanted archive is deleted");
    }

    /// Confirm hands the request to a backend that may be busy, so Cancel can
    /// come before anything has answered.
    #[test]
    fn canceling_before_the_backend_answers_a_confirmed_link_opens_nothing_later() {
        let ui = test_window();
        let (commands, received) = std::sync::mpsc::channel();
        let import_flow = Rc::new(RefCell::new(ImportFlowState::default()));
        pack_link_cancel_wiring(&ui, &commands);
        let base = tempfile::tempdir().expect("a disposable root");
        let archive = archive_in(&base, "link.pack");
        ui.set_modal("open-pack-link".into());
        let generation = begin_pack_link_request(&ui);

        ui.invoke_cancel_pack_link();
        ui.set_modal("rename".into());
        assert!(matches!(
            received.try_recv(),
            Ok(Command::CancelPackUrl { generation: stopped }) if stopped == generation
        ));

        for event in [
            Event::PackUrlDownloading {
                generation,
                url: "https://packs.example/pack.mrpack".to_owned(),
                display: "https://packs.example/pack.mrpack".to_owned(),
                host: "packs.example".to_owned(),
            },
            Event::PackDownloaded {
                generation,
                archive: archive.clone(),
            },
        ] {
            apply_pack_link_event(&ui, event, &import_flow, &commands);
        }

        assert_eq!(ui.get_modal(), "rename", "the user's dialog is left alone");
        assert!(received.try_recv().is_err());
        assert!(!archive.exists());
    }

    #[test]
    fn a_finished_link_download_opens_the_import() {
        let ui = test_window();
        let (commands, received) = std::sync::mpsc::channel();
        let import_flow = Rc::new(RefCell::new(ImportFlowState::default()));
        pack_link_cancel_wiring(&ui, &commands);
        let generation = pack_link_downloading(&ui, &commands, &import_flow);

        apply_pack_link_event(
            &ui,
            Event::PackDownloaded {
                generation,
                archive: PathBuf::from("link.pack"),
            },
            &import_flow,
            &commands,
        );

        assert_eq!(ui.get_modal(), "import");
        assert!(!ui.get_pack_link_downloading());
        assert!(matches!(
            received.try_recv(),
            Ok(Command::AnalyzeImport { .. })
        ));
    }

    #[test]
    fn leaving_the_link_dialog_by_any_route_cancels_its_download() {
        let ui = test_window();
        let (commands, received) = std::sync::mpsc::channel();
        let import_flow = Rc::new(RefCell::new(ImportFlowState::default()));
        pack_link_cancel_wiring(&ui, &commands);
        let generation = pack_link_downloading(&ui, &commands, &import_flow);

        assert!(make_room_for_dialog(&ui));

        assert!(matches!(
            received.try_recv(),
            Ok(Command::CancelPackUrl { generation: stopped }) if stopped == generation
        ));
        assert!(!ui.get_pack_link_downloading());
    }

    #[test]
    fn events_of_a_replaced_link_download_are_ignored() {
        let ui = test_window();
        let (commands, received) = std::sync::mpsc::channel();
        let import_flow = Rc::new(RefCell::new(ImportFlowState::default()));
        pack_link_cancel_wiring(&ui, &commands);
        let base = tempfile::tempdir().expect("a disposable root");
        let archive = archive_in(&base, "old.pack");
        let older = pack_link_downloading(&ui, &commands, &import_flow);
        let newer = begin_pack_link_request(&ui);
        assert!(newer > older);

        apply_pack_link_event(
            &ui,
            Event::PackDownloaded {
                generation: older,
                archive: archive.clone(),
            },
            &import_flow,
            &commands,
        );
        apply_pack_link_event(
            &ui,
            Event::PackUrlEnded {
                generation: older,
                message: Some("old failure".to_owned()),
            },
            &import_flow,
            &commands,
        );

        assert_eq!(ui.get_modal(), "open-pack-link");
        assert!(
            ui.get_pack_link_downloading(),
            "the newer link keeps downloading"
        );
        assert_eq!(ui.get_modal_error(), "");
        assert!(received.try_recv().is_err());
        assert!(!archive.exists());
    }

    #[test]
    fn a_redirect_question_for_a_download_the_user_canceled_is_not_asked() {
        let ui = test_window();
        let (commands, _received) = std::sync::mpsc::channel();
        let import_flow = Rc::new(RefCell::new(ImportFlowState::default()));
        pack_link_cancel_wiring(&ui, &commands);
        let generation = pack_link_downloading(&ui, &commands, &import_flow);
        ui.invoke_cancel_pack_link();
        ui.set_modal(SharedString::default());

        apply_pack_link_event(
            &ui,
            Event::PackUrlConsent {
                generation,
                url: "https://packs.example/pack.mrpack".to_owned(),
                display: "https://cdn.example/pack.mrpack".to_owned(),
                host: "cdn.example".to_owned(),
                redirected: true,
            },
            &import_flow,
            &commands,
        );

        assert_eq!(ui.get_modal(), "");
    }

    #[test]
    fn a_failed_link_download_shows_why_in_its_dialog() {
        let ui = test_window();
        let (commands, _received) = std::sync::mpsc::channel();
        let import_flow = Rc::new(RefCell::new(ImportFlowState::default()));
        pack_link_cancel_wiring(&ui, &commands);
        let generation = pack_link_downloading(&ui, &commands, &import_flow);

        apply_pack_link_event(
            &ui,
            Event::PackUrlEnded {
                generation,
                message: Some("The download failed".to_owned()),
            },
            &import_flow,
            &commands,
        );

        assert_eq!(ui.get_modal(), "open-pack-link");
        assert_eq!(ui.get_modal_error(), "The download failed");
        assert!(!ui.get_pack_link_downloading());
    }

    #[test]
    fn a_link_download_canceled_elsewhere_closes_its_dialog() {
        let ui = test_window();
        let (commands, _received) = std::sync::mpsc::channel();
        let import_flow = Rc::new(RefCell::new(ImportFlowState::default()));
        pack_link_cancel_wiring(&ui, &commands);
        let generation = pack_link_downloading(&ui, &commands, &import_flow);

        apply_pack_link_event(
            &ui,
            Event::PackUrlEnded {
                generation,
                message: None,
            },
            &import_flow,
            &commands,
        );

        assert_eq!(ui.get_modal(), "");
        assert!(!ui.get_pack_link_downloading());
    }

    #[test]
    fn a_requested_dialog_over_an_install_confirmation_or_import_lets_go_of_it() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        open_install_confirmation(&ui, &workspace);

        assert!(make_room_for_dialog(&ui));
        assert!(workspace.browser.installs.borrow().active.is_none());

        let closed = Rc::new(Cell::new(false));
        let seen = Rc::clone(&closed);
        ui.on_close_import_modal(move || seen.set(true));
        ui.set_modal("import".into());
        assert!(make_room_for_dialog(&ui));
        assert!(closed.get(), "the import stops reading its archive");
    }

    #[test]
    fn a_requested_dialog_never_replaces_a_manual_download_prompt() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        let toasts = toasts(&ui);
        let host = ephervex_model::InstanceId::new().to_string();
        apply_mod_acquisition(&ui, acquisition_prompt(1, &host), &workspace);

        assert!(!make_room_for_dialog(&ui));

        assert_eq!(ui.get_modal(), "mod-acquire");
        assert_eq!(
            toasts.borrow().as_slice(),
            [(lookup("gui-dialog-not-shown"), true)]
        );
    }

    #[test]
    fn a_confirmed_operations_completion_leaves_a_dialog_opened_since() {
        let ui = test_window();
        ui.set_modal("snapshot-create".into());

        close_modal_for(&ui, backend::ModalScope::NONE);

        assert_eq!(ui.get_modal(), "snapshot-create");
    }

    #[test]
    fn a_mod_impact_question_never_replaces_a_manual_download_prompt() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        let toasts = toasts(&ui);
        let host = ephervex_model::InstanceId::new().to_string();
        apply_mod_acquisition(&ui, acquisition_prompt(1, &host), &workspace);

        apply_mod_impact(&ui, &ModImpactPresentation::default());

        assert_eq!(ui.get_modal(), "mod-acquire");
        assert_eq!(
            toasts.borrow().as_slice(),
            [(lookup("gui-dialog-not-shown"), true)]
        );
    }

    #[test]
    fn a_mod_impact_question_over_an_install_confirmation_releases_the_install_queue() {
        let ui = test_window();
        let (workspace, _commands) = test_workspace_sending(&ui);
        open_install_confirmation(&ui, &workspace);

        apply_mod_impact(&ui, &ModImpactPresentation::default());

        assert_eq!(ui.get_modal(), "mod-impact");
        assert!(workspace.browser.installs.borrow().active.is_none());
    }

    #[test]
    fn a_late_mod_plan_never_replaces_a_manual_download_prompt() {
        let ui = test_window();
        let toasts = toasts(&ui);
        let (workspace, commands) = test_workspace_sending(&ui);
        register_mod_mutations(&ui, &workspace.commands, &workspace);
        let host = ephervex_model::InstanceId::new().to_string();
        ui.invoke_plan_mod_removal("client".into(), host.as_str().into(), "mods/m.jar".into());
        let card = requested_plan(&commands);
        let apply = ask_from_manager(&ui, &workspace, &commands, &host, PlanKind::Apply);
        apply_mod_acquisition(&ui, acquisition_prompt(1, &host), &workspace);

        apply_mod_removal_plan(&ui, removal_plan(card, &host), &workspace);
        apply_linked_confirmation(
            &ui,
            apply,
            backend::LinkedSectionData::default(),
            &workspace,
        );

        assert_eq!(ui.get_modal(), "mod-acquire");
        assert_eq!(
            toasts.borrow().as_slice(),
            [
                (lookup("gui-dialog-not-shown"), true),
                (lookup("gui-dialog-not-shown"), true)
            ],
            "the user is told each request was not shown"
        );
    }

    #[test]
    fn a_manage_removal_plan_never_replaces_a_manual_download_prompt() {
        let ui = test_window();
        let toasts = toasts(&ui);
        let (workspace, commands) = test_workspace_sending(&ui);
        let host = ephervex_model::InstanceId::new().to_string();
        let request = ask_from_manager(&ui, &workspace, &commands, &host, PlanKind::Removal);
        apply_mod_acquisition(&ui, acquisition_prompt(1, &host), &workspace);

        apply_mod_removal_plan(&ui, removal_plan(request, &host), &workspace);

        assert_eq!(ui.get_modal(), "mod-acquire");
        assert_eq!(
            toasts.borrow().as_slice(),
            [(lookup("gui-dialog-not-shown"), true)]
        );
    }

    #[test]
    fn leaving_a_manual_download_prompt_cancels_the_wait_it_names() {
        let ui = test_window();
        let (workspace, commands) = test_workspace_sending(&ui);
        register_mod_actions(&ui, &workspace.commands, &workspace);
        let host = ephervex_model::InstanceId::new().to_string();
        apply_mod_acquisition(&ui, acquisition_prompt(7, &host), &workspace);

        ui.invoke_cancel_mod_acquisition(ui.get_mod_acquisition_id());

        assert!(matches!(
            commands.try_recv(),
            Ok(Command::CancelModAcquisition { id: 7 })
        ));
    }

    #[test]
    fn the_suggested_name_follows_a_release_that_drops_the_selected_loader() {
        let ui = test_window();

        ui.set_create_minecraft("1.21.1".into());
        ui.set_create_loader("NeoForge".into());
        ui.set_create_loader_version("21.1.249".into());
        recompute_create_name(&ui);
        assert_eq!(ui.get_create_name_suggestion(), "1.21.1-NeoForge-21.1.249");

        // 1.19.4 predates NeoForge's first build, so choosing it resets the
        // loader. The suggestion has to follow, or the dialog offers a name
        // describing a loader it no longer holds.
        ui.set_create_minecraft("1.19.4".into());
        apply_minecraft_selection(&ui, "client", "1.19.4");

        assert_eq!(ui.get_create_loader(), "Vanilla");
        assert_eq!(ui.get_create_loader_version(), "");
        // A vanilla instance is named after its release alone.
        assert_eq!(ui.get_create_name_suggestion(), "1.19.4");
    }

    #[test]
    fn a_name_the_user_typed_is_never_replaced_by_a_suggestion() {
        let ui = test_window();

        ui.set_create_minecraft("1.19.4".into());
        ui.set_create_loader("NeoForge".into());
        ui.set_create_name("Crash repro".into());
        apply_minecraft_selection(&ui, "client", "1.19.4");

        assert_eq!(ui.get_create_loader(), "Vanilla");
        assert_eq!(
            ui.get_create_name(),
            "Crash repro",
            "the suggestion is a placeholder, so it can never overwrite the field"
        );
        assert_eq!(ui.get_create_name_suggestion(), "1.19.4");
    }

    #[test]
    fn the_install_queue_runs_one_request_at_a_time_in_the_order_they_were_pressed() {
        let request = |project: &str| BrowserInstall {
            server: false,
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
            server: false,
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
        let source = ui_markup();
        assert!(source.contains(
            "open-workspace(id) => { root.selected-instance-id = id; root.selected-instance-index = index; root.select-instance(id); root.open-workspace(\"client\", id, \"Overview\");"
        ));
        assert!(source.contains(
            "open-workspace(id) => { root.selected-global-server-id = id; root.selected-global-server-index = index; root.select-server(id); root.open-workspace(\"server\", id, \"Overview\");"
        ));
        // A card's own touch area has to be declared before the layout holding
        // its buttons, so that it sits under them. Slint resolves a pointer
        // event front to back, so a touch area declared after the buttons takes
        // the press first and the button never sees it. What that costs the
        // user is proven in `tests/card_pointer_targets.rs`; this only keeps the
        // arrangement visible where it is written.
        for component in ["component InstanceCard", "component ServerCard"] {
            let card = ui_declaration(source, component).expect("the card component");
            let area = card
                .find("card-area := ContextTouchArea")
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
        let source = ui_markup();
        // One axis: the value is the override. There is no second control
        // asking whether the value the user just chose should count.
        assert!(source.contains("checked: root.item.bool-value;"));
        // The management control this replaces must be gone from the row,
        // along with every property that fed it.
        let row = ui_declaration(
            source,
            "component MinecraftPreferenceRowCard inherits Rectangle",
        )
        .expect("the preference row component");
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
        let source = ui_markup();
        assert!(source.contains("component ActivityCard inherits Rectangle"));
        assert!(source.contains("height: max(102px, detail-text.preferred-height + 82px)"));
        assert!(source.contains("detail-text := BodyText { text: item.detail; visible: item.detail != \"\"; wrap: word-wrap; }"));
    }

    #[test]
    fn global_navigation_keeps_diagnostics_contextual() {
        let source = ui_markup();
        let logs = "gui-logs";
        assert!(!source.contains(&format!(
            r#"label: GuiI18n.text("{logs}"); selected: root.page == 2"#
        )));
        // Logs is a surface of each host's workspace, read through that pane's viewer.
        assert!(source.contains("if host.available && pane.surface == \"Logs\""));
        assert!(source.contains("root.log-show(pane.id, \"files\")"));
    }

    #[test]
    fn snapshot_management_is_contextual_for_clients_and_servers() {
        let source = ui_markup();
        let controller = include_str!("controller.rs");
        let replacement_note = "gui-snapshot-replacement-note";
        // Opening the surface loads the snapshots of the host its pane shows.
        assert!(controller.contains("ui.invoke_load_snapshots(kind, id);"));
        assert!(source.contains("root.snapshot-safety-copy = true"));
        assert!(source.contains(&format!("GuiI18n.text(\"{replacement_note}\")")));
        assert!(source.contains("host.snapshots-have-plain-copies"));
        assert_eq!(
            source
                .matches("root.pending-snapshot-deletion-note = ")
                .count(),
            6,
            "every Delete, including the ones a narrow overview moves to a row of their own"
        );
        assert!(source.contains("text: root.pending-snapshot-deletion-note"));
    }

    #[test]
    fn wide_forms_and_server_details_use_hard_readability_bounds() {
        let source = ui_markup();
        assert!(source.contains("width: min(parent.width, 980px)"));
        assert!(source.contains("width: min(parent.width, 1100px)"));
        assert!(!source.contains("max-width: 980px"));
        assert!(!source.contains("max-width: 1100px"));
    }

    #[test]
    fn settings_use_scrollable_reusable_preference_rows() {
        let source = ui_markup();
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
        let source = ui_markup();
        let row_component = ui_declaration(
            source,
            "component MinecraftPreferenceRowCard inherits Rectangle",
        )
        .expect("the preference row component");
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
        let source = ui_markup();
        // The picker is an actual window the window manager sizes, so what the
        // user sees on screen is the window they are asking Minecraft for.
        assert!(source.contains("export component WindowSizePreview inherits Window {"));
        assert!(
            !source.contains("WindowSizeCanvas"),
            "the drawn stand-in it replaces must be gone"
        );
        let controller = include_str!("controller.rs");
        let production = controller.split("#[cfg(test)]").next().unwrap_or_default();
        let preview = ui_declaration(
            source,
            "export component WindowSizePreview inherits Window {",
        )
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

        let row_component = ui_declaration(
            source,
            "component MinecraftPreferenceRowCard inherits Rectangle",
        )
        .expect("the preference row component");
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
        assert!(production.contains("shown.set_row_data(index, row);"));
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

    /// A crash report is tab indented on nearly every stack line, so a viewer
    /// that shows the file as it is draws a column of missing-character boxes
    /// down the left of the report.
    #[test]
    fn a_crash_report_reaches_its_viewer_with_no_tab_left_in_it() {
        let ui = test_window();
        let (sender, _commands) = std::sync::mpsc::channel();
        let mut logs = LogViews::new(sender);
        logs.attach(&ui);
        let instance = ephervex_model::InstanceId::new();
        let pane = PaneId::from_raw(1);
        let name = "crash-2026-09-18_16.54.00-client.txt";
        logs.sync(&[(pane, InstalledHost::Instance(instance), Surface::Logs)]);
        logs.show(pane, log_views::LogMode::Crashes);
        logs.open_file(pane, name);
        apply_diagnostic_content(
            &ui,
            &mut logs,
            &DiagnosticContent {
                pane,
                scope: ephervex_core::DiagnosticScope::Client(instance),
                kind: backend::parse_diagnostic_kind("crash-report").expect("kind"),
                name: name.to_owned(),
                title: "crash-2026-09-18_16.54.00-client.txt".to_owned(),
                path: "/tmp/crash-reports/crash-2026-09-18_16.54.00-client.txt".to_owned(),
                text: "java.lang.Throwable\n\tat net.minecraft.Util.run(Util.java:12)\n".to_owned(),
                truncated: false,
                lossy: false,
                matches: None,
            },
        );
        let shown = ui.get_log_views().row_data(0).expect("viewer").report;
        assert!(
            !shown.contains('\t'),
            "a tab left in the report has no glyph"
        );
        assert!(
            shown.contains("\n    at net.minecraft.Util.run"),
            "the indentation the report carries has to survive as spaces: {shown}"
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
        let source = ui_markup();
        assert!(source.contains("accessible-read-only: true"));
        assert!(source.contains("read-only: true"));
        assert!(source.contains("selectable-input.copy()"));
        assert!(source.contains("selectable-input.select-all()"));
        assert!(source.contains(
            "SelectableText { text: log-view.report; font-size: root.technical-text-size * 1px; }"
        ));

        assert!(source.contains("component LiveLogViewer inherits Rectangle"));
        assert!(source.contains("root.log-copy(pane.id)"));
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
                generation: None,
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
                generation: None,
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
                generation: None,
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
                    generation: None,
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
        let source = ui_markup();
        assert!(source.contains("in-out property <float> text-scale: 1"));
        assert!(source.contains("default-font-size: Theme.body-size"));
        // Saved with the rest of the General page, by its one button.
        assert!(
            source.contains("callback save-general-settings(int, int, string, bool, bool, int)")
        );
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

    /// A name that opens a web page has to look like one before it is pressed.
    /// The press itself is covered by the `mod_version_badges` suite; what is
    /// asserted here is the affordance, which is invisible to a click test.
    #[test]
    fn the_mod_name_that_opens_a_project_page_is_marked_as_a_link() {
        let source = ui_markup();
        let header = source
            .split("if root.modal == \"mod-manage\"")
            .nth(1)
            .unwrap_or_default();
        assert!(
            header.contains("external-link.svg"),
            "the arrow that says a name can be pressed"
        );
        assert!(header.contains("mouse-cursor: pointer"));
        assert!(
            header.contains("gui-mod-open-project-page-hint"),
            "the tooltip naming where it leads"
        );
    }

    #[test]
    fn linked_server_workspace_and_pending_creation_remain_contextual() {
        let source = ui_markup();
        assert!(source.contains("for server in host.linked-servers"));
        assert!(source.contains("root.follow-host(pane.id, \"server\", server.id, \"Overview\")"));
        // Scoped by the model it renders rather than by hiding rows, which
        // would leave the room they would have taken behind them.
        assert!(source.contains("for pending in host.pending-servers"));
        assert!(source.contains("NameRules.linked-server-name(name)"));
    }

    #[test]
    fn provider_and_storage_resets_use_explicit_application_callbacks() {
        let source = ui_markup();
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
        let source = ui_markup();
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
        assert!(source.contains("root.settings-tab == \"Providers & APIs\""));
    }

    #[test]
    fn the_preferred_catalog_is_applied_when_an_instance_workspace_opens() {
        let source = include_str!("controller.rs");
        // The lifecycle point the preference applies at, and the same reset a
        // chip click uses, so an in-flight search cannot survive the move.
        assert!(source.contains("presentation::catalog_to_open_on(&preferred, default)"));
        assert!(
            include_str!("controller/mod_views.rs")
                .contains("fn switch(&mut self, provider: &str)")
        );
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
        let source = ui_markup();
        // Presence is the only thing the view is told, and the three operations
        // are separate callbacks so removing cannot be expressed as saving an
        // empty value.
        assert!(source.contains("in-out property <bool> curseforge-key-configured"));
        assert!(source.contains("callback save-curseforge-api-key(string)"));
        assert!(source.contains("callback remove-curseforge-api-key()"));
        assert!(source.contains("callback open-curseforge-key-help()"));
        assert!(source.contains("gui-curseforge-key-configured"));
        assert!(source.contains("gui-curseforge-key-not-configured"));
        assert!(source.contains("gui-curseforge-key-default-in-use"));
        // Replacing asks for the new value rather than revealing the old one.
        assert!(source.contains("gui-curseforge-key-replace"));
        assert!(source.contains("input-type: InputType.password"));
        // The button that removes a credential must only appear once the user
        // actually has one to remove - never for this build's own fallback key.
        assert!(source.contains("if root.curseforge-key-configured : AppButton"));
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
            "curseforge_key_configured(settings.curseforge_key.is_configured())"
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
    fn the_cache_section_measures_on_demand_and_only_ever_reviews_a_cleanup() {
        let source = ui_markup();
        // Measuring walks the store, so it must not be on the ordinary refresh
        // path — only on opening the page that shows the figure.
        assert!(source.contains("root.measure-artifact-cache()"));
        // The page offers a review of what a cleanup would remove and nothing
        // that removes without one.
        assert!(source.contains("root.review-storage-cleanup()"));
        assert!(!source.contains("clear-artifact-cache"));
        assert!(source.contains("callback confirm-storage-cleanup(int)"));
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
        assert!(
            !backend.contains("clear_artifact_cache"),
            "no frontend path removes the cache without a reviewed plan"
        );
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
    fn a_picked_archive_is_analyzed_only_while_its_attempt_is_current() {
        let mut flow = ImportFlowState::default();
        let first = flow.begin_attempt();
        assert!(flow.tracks(first));

        let second = flow.begin_attempt();
        assert!(!flow.tracks(first), "a newer attempt superseded it");
        assert!(flow.tracks(second));

        flow.reset();
        assert!(!flow.tracks(second), "closing the modal ends the attempt");
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
    fn a_second_import_of_an_archive_still_importing_is_refused_and_leaves_the_dialog_usable() {
        let mut creations = CreationTracker::default();
        let mut flow = ImportFlowState::default();
        let analyze = |flow: &mut ImportFlowState| {
            let generation = flow.begin_attempt();
            flow.analyzed(
                generation,
                PathBuf::from("pack.mrpack"),
                Ok(resolved_presentation("Example Pack", false)),
            );
        };

        analyze(&mut flow);
        let first = flow.begin_submit().expect("the analyzed pack is ready");
        let request_id = register_import_creation(&mut flow, &mut creations, &first, false);
        assert!(request_id.is_some(), "the first import gets a card");
        assert_eq!(creations.pending.len(), 1);

        // The dialog is opened again for the same file while that import runs.
        flow.reset();
        analyze(&mut flow);
        let second = flow.begin_submit().expect("the analyzed pack is ready");
        assert_eq!(flow.phase, ImportPhase::Submitting);
        assert_eq!(
            register_import_creation(&mut flow, &mut creations, &second, false),
            None,
            "no second import may start while the archive is still importing"
        );
        assert_eq!(creations.pending.len(), 1, "no second card appears");
        assert!(
            flow.is_ready(),
            "the refused submission must not leave the dialog stuck in Submitting"
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

    fn impact_row(name: &str) -> crate::presentation::ImpactRowPresentation {
        crate::presentation::ImpactRowPresentation {
            name: name.to_owned(),
            logical_path: format!("mods/{name}.jar"),
            distance: 1,
        }
    }

    fn turn_off_impact(
        host: &str,
        unknown: Vec<crate::presentation::ImpactRowPresentation>,
    ) -> ModImpactPresentation {
        ModImpactPresentation {
            host_kind: "client".to_owned(),
            host_id: host.to_owned(),
            path: "mods/LibX.jar".to_owned(),
            name: "LibX".to_owned(),
            deleting: false,
            breaks: Vec::new(),
            degrades: Vec::new(),
            unknown,
            unidentifiable: Vec::new(),
        }
    }

    /// Applies the open dialog and returns the command the controller sent.
    fn apply_open_dialog(ui: &AppWindow, cascade: bool) -> Command {
        let (sender, commands) = std::sync::mpsc::channel();
        register_dependency_actions(ui, &sender);
        ui.invoke_apply_mod_impact(cascade);
        commands.try_recv().expect("the dialog sends one command")
    }

    /// The turn-off dialog opens whenever a dependent is merely unknown, and
    /// nothing in its lists then says which way the user is going.
    #[test]
    fn confirming_a_turn_off_with_nothing_proven_broken_turns_the_mod_off() {
        let ui = test_window();
        let host = ephervex_model::InstanceId::new().to_string();
        apply_mod_impact(&ui, &turn_off_impact(&host, vec![impact_row("Mystery")]));

        assert!(!ui.get_mod_impact_enabling());
        let Command::SetModsEnabled { changes, .. } = apply_open_dialog(&ui, true) else {
            panic!("a turn-off sends the enable change");
        };
        assert_eq!(changes, vec![(PathBuf::from("mods/LibX.jar"), false)]);
    }

    #[test]
    fn confirming_a_turn_off_with_proven_dependents_turns_them_off_too() {
        let ui = test_window();
        let host = ephervex_model::InstanceId::new().to_string();
        let mut impact = turn_off_impact(&host, Vec::new());
        impact.breaks = vec![impact_row("Sky GUIs")];
        apply_mod_impact(&ui, &impact);

        let Command::SetModsEnabled { changes, .. } = apply_open_dialog(&ui, true) else {
            panic!("a turn-off sends the enable change");
        };
        assert_eq!(
            changes,
            vec![
                (PathBuf::from("mods/Sky GUIs.jar"), false),
                (PathBuf::from("mods/LibX.jar"), false),
            ]
        );
    }

    #[test]
    fn confirming_a_turn_on_enables_the_mod_and_what_it_needs() {
        let ui = test_window();
        let host = ephervex_model::InstanceId::new().to_string();
        apply_mod_requirements(
            &ui,
            &ModRequirementPresentation {
                host_kind: "client".to_owned(),
                host_id: host,
                path: "mods/Addon.jar".to_owned(),
                name: "Addon".to_owned(),
                disabled: vec![impact_row("LibX")],
                absent: Vec::new(),
                incompatible: Vec::new(),
            },
        );

        assert!(ui.get_mod_impact_enabling());
        let Command::SetModsEnabled { changes, .. } = apply_open_dialog(&ui, true) else {
            panic!("a turn-on sends the enable change");
        };
        assert_eq!(
            changes,
            vec![
                (PathBuf::from("mods/LibX.jar"), true),
                (PathBuf::from("mods/Addon.jar"), true),
            ]
        );
    }

    /// Absent mods are only reported, so the lists that can be cascaded are empty.
    #[test]
    fn confirming_a_turn_on_that_only_reports_absent_mods_still_turns_the_mod_on() {
        let ui = test_window();
        let host = ephervex_model::InstanceId::new().to_string();
        apply_mod_requirements(
            &ui,
            &ModRequirementPresentation {
                host_kind: "client".to_owned(),
                host_id: host,
                path: "mods/Addon.jar".to_owned(),
                name: "Addon".to_owned(),
                disabled: Vec::new(),
                absent: vec![impact_row("Missing")],
                incompatible: Vec::new(),
            },
        );

        let Command::SetModsEnabled { changes, .. } = apply_open_dialog(&ui, true) else {
            panic!("a turn-on sends the enable change");
        };
        assert_eq!(changes, vec![(PathBuf::from("mods/Addon.jar"), true)]);
    }

    /// A window with the Minecraft defaults wired up and a capture open on
    /// one row, so the press reader is armed.
    fn window_with_open_capture() -> (
        AppWindow,
        BindingCapture,
        Rc<RefCell<MinecraftDefaultsModel>>,
        std::sync::mpsc::Receiver<Command>,
    ) {
        let ui = test_window();
        let (sender, commands) = std::sync::mpsc::channel();
        let minecraft = Rc::new(RefCell::new(MinecraftDefaultsModel::new()));
        let keys = BindingCapture::default();
        register_minecraft_defaults(&ui, &sender, &minecraft, &keys);

        ui.invoke_minecraft_begin_capture("keybind_sneak".into());
        assert!(keys.is_armed());
        assert_eq!(ui.get_minecraft_capturing_id(), "keybind_sneak");
        (ui, keys, minecraft, commands)
    }

    fn assert_capture_ended(
        ui: &AppWindow,
        keys: &BindingCapture,
        minecraft: &Rc<RefCell<MinecraftDefaultsModel>>,
    ) {
        assert!(!keys.is_armed(), "the reader must stop taking presses");
        assert_eq!(ui.get_minecraft_capturing_id(), "");
        assert_eq!(minecraft.borrow().capturing(), None);
    }

    #[test]
    fn changing_the_visible_rows_ends_an_open_key_capture() {
        let (ui, keys, minecraft, _commands) = window_with_open_capture();
        ui.invoke_minecraft_select_category(0);
        assert_capture_ended(&ui, &keys, &minecraft);

        let (ui, keys, minecraft, _commands) = window_with_open_capture();
        ui.invoke_minecraft_set_query("sneak".into());
        assert_capture_ended(&ui, &keys, &minecraft);

        let (ui, keys, minecraft, _commands) = window_with_open_capture();
        ui.invoke_minecraft_set_show_advanced(true);
        assert_capture_ended(&ui, &keys, &minecraft);

        let (ui, keys, minecraft, _commands) = window_with_open_capture();
        ui.invoke_minecraft_set_only_overridden(true);
        assert_capture_ended(&ui, &keys, &minecraft);
    }

    #[test]
    fn discarding_the_draft_ends_an_open_key_capture() {
        let (ui, keys, minecraft, _commands) = window_with_open_capture();
        ui.invoke_minecraft_discard_defaults();
        assert_capture_ended(&ui, &keys, &minecraft);
    }

    #[test]
    fn forgetting_a_preference_ends_an_open_key_capture() {
        let (ui, keys, minecraft, _commands) = window_with_open_capture();
        ui.invoke_minecraft_forget_preference("keybind_sneak".into());
        assert_capture_ended(&ui, &keys, &minecraft);
    }

    #[test]
    fn canceling_an_open_key_capture_stops_the_reader() {
        let (ui, keys, minecraft, _commands) = window_with_open_capture();
        ui.invoke_minecraft_cancel_capture();
        assert_capture_ended(&ui, &keys, &minecraft);
    }

    #[test]
    fn typed_custom_lines_are_saved_with_the_batch_and_listed_when_unwritable() {
        let ui = test_window();
        let (sender, commands) = std::sync::mpsc::channel();
        let minecraft = Rc::new(RefCell::new(MinecraftDefaultsModel::new()));
        register_minecraft_defaults(&ui, &sender, &minecraft, &BindingCapture::default());
        assert!(!ui.get_minecraft_can_save());

        ui.invoke_minecraft_edit_raw_options("autoJump:true\nnot a line".into());

        assert!(ui.get_minecraft_dirty());
        assert!(ui.get_minecraft_can_save());
        assert!(ui.get_minecraft_raw_problems().contains("Line 2"));

        ui.invoke_minecraft_save_defaults();
        let Command::SaveMinecraftPreferences { edits, raw_options } =
            commands.try_recv().expect("a save is sent")
        else {
            panic!("saving sends the preference batch");
        };
        assert!(edits.is_empty());
        assert_eq!(raw_options.as_deref(), Some("autoJump:true\nnot a line"));
        assert!(ui.get_minecraft_saving());

        ui.invoke_minecraft_discard_defaults();
        assert_eq!(ui.get_minecraft_raw_options(), "");
    }

    #[test]
    fn a_press_with_no_capturing_row_disarms_the_reader() {
        let (ui, keys, _minecraft, _commands) = window_with_open_capture();
        // The row went away without any handler running.
        ui.set_minecraft_capturing_id(SharedString::default());
        assert!(keys.is_armed());

        keys.deliver_press(CapturedInput::Mouse(ephervex_core::MouseButton::Left));
        assert!(!keys.is_armed());
    }

    #[test]
    fn leaving_the_page_that_shows_the_capturing_row_ends_the_capture() {
        let markup = ui_markup();
        let changed = |property: &str| {
            let start = markup
                .find(&format!("changed {property} =>"))
                .unwrap_or_else(|| panic!("the window reacts to {property} changing"));
            let body = &markup[start..];
            let body = &body[..body.find("\n    }").expect("a closed handler")];
            body.contains("minecraft-cancel-capture")
        };
        assert!(changed("settings-tab"));
        assert!(changed("page"));
    }

    /// Tall enough to put every row of a category on screen, which is where a
    /// query can find its controls.
    const SCREEN_HEIGHT: u32 = 9000;

    /// More Tab presses than the busiest window has focusable controls.
    const TAB_LIMIT: usize = 400;

    /// The Minecraft defaults screen as the app wires it, with the shipped
    /// catalog loaded and the first row of the given editor kind on screen.
    struct MinecraftScreen {
        ui: AppWindow,
        minecraft: Rc<RefCell<MinecraftDefaultsModel>>,
        row: MinecraftPreferenceRow,
        /// The row listed right after `row`, when the screen was asked for one.
        following: Option<MinecraftPreferenceRow>,
        sender: std::sync::mpsc::Sender<Command>,
        // The wiring holds clones of the capture; keeping it here keeps it
        // alive for the whole test.
        _keys: BindingCapture,
        commands: std::sync::mpsc::Receiver<Command>,
    }

    impl MinecraftScreen {
        fn showing(editor: &str) -> Self {
            Self::showing_where(|rows| rows.iter().position(|row| row.editor == editor))
        }

        /// A text field row with a row after it that a key can change, so the
        /// control Tab leaves the field for can be told from a lost focus.
        fn showing_field_before_a_keyboard_control() -> Self {
            let mut screen = Self::showing_where(|rows| {
                rows.windows(2).position(|pair| {
                    matches!(pair[0].editor.as_str(), "number" | "text")
                        && matches!(pair[1].editor.as_str(), "toggle" | "slider" | "enum")
                })
            });
            let rows = screen.minecraft.borrow().rows();
            let at = rows
                .iter()
                .position(|row| row.id == screen.row.id)
                .expect("the row is listed");
            screen.following = rows.get(at + 1).cloned();
            screen
        }

        /// The ids of every slider row the shipped catalog lists.
        fn slider_ids() -> Vec<String> {
            let mut minecraft = MinecraftDefaultsModel::new();
            let catalog = ephervex_core::MinecraftPreferenceCatalog::bundled_shared()
                .expect("the bundled catalog");
            minecraft.apply_snapshot(ephervex_core::MinecraftPreferenceSnapshot {
                catalog_minecraft_version: catalog.canonical_minecraft_version.clone(),
                entries: catalog
                    .descriptors
                    .iter()
                    .map(|descriptor| ephervex_core::MinecraftPreferenceEntry {
                        descriptor: descriptor.clone(),
                        managed: false,
                        value: None,
                    })
                    .collect(),
                unusable: Vec::new(),
                unrecognized: 0,
                raw_options: String::new(),
            });
            minecraft.set_show_advanced(true);
            minecraft.set_preview_release(&catalog.canonical_minecraft_version);
            (0..minecraft_defaults::CATEGORIES.len())
                .flat_map(|category| {
                    minecraft.select_category(category);
                    minecraft
                        .rows()
                        .iter()
                        .filter(|row| row.editor == "slider")
                        .map(|row| row.id.to_string())
                        .collect::<Vec<_>>()
                })
                .collect()
        }

        fn showing_where(select: impl Fn(&[MinecraftPreferenceRow]) -> Option<usize>) -> Self {
            let ui = test_window();
            let (sender, commands) = std::sync::mpsc::channel();
            let minecraft = Rc::new(RefCell::new(MinecraftDefaultsModel::new()));
            let keys = BindingCapture::default();
            register_minecraft_defaults(&ui, &sender, &minecraft, &keys);

            let catalog = ephervex_core::MinecraftPreferenceCatalog::bundled_shared()
                .expect("the bundled catalog");
            minecraft
                .borrow_mut()
                .apply_snapshot(ephervex_core::MinecraftPreferenceSnapshot {
                    catalog_minecraft_version: catalog.canonical_minecraft_version.clone(),
                    entries: catalog
                        .descriptors
                        .iter()
                        .map(|descriptor| ephervex_core::MinecraftPreferenceEntry {
                            descriptor: descriptor.clone(),
                            managed: false,
                            value: None,
                        })
                        .collect(),
                    unusable: Vec::new(),
                    unrecognized: 0,
                    raw_options: String::new(),
                });
            minecraft.borrow_mut().set_show_advanced(true);
            minecraft
                .borrow_mut()
                .set_preview_release(&catalog.canonical_minecraft_version);
            let row = (0..minecraft_defaults::CATEGORIES.len())
                .find_map(|category| {
                    minecraft.borrow_mut().select_category(category);
                    let rows = minecraft.borrow().rows();
                    select(&rows).map(|index| rows[index].clone())
                })
                .expect("the catalog has a row of the wanted kind");
            render_minecraft(&ui, &minecraft);

            ui.window()
                .set_size(slint::PhysicalSize::new(1480, SCREEN_HEIGHT));
            ui.set_page(4);
            ui.set_settings_tab("Minecraft".into());
            ui.show().expect("show");
            slint::platform::update_timers_and_animations();
            Self {
                ui,
                minecraft,
                row,
                following: None,
                sender,
                _keys: keys,
                commands,
            }
        }

        /// Delivers the answer to the newest preview request, as the backend
        /// does some time after the edit that asked for it.
        fn answer_preview(&self) {
            let generation = self
                .commands
                .try_iter()
                .filter_map(|command| match command {
                    Command::PreviewMinecraftPreferences { generation, .. } => Some(generation),
                    _ => None,
                })
                .last()
                .expect("an edit asks for a preview");
            let release = self.minecraft.borrow().preview_release().to_owned();
            let preview = ephervex_core::MinecraftPreferencePreview {
                minecraft_version: release.clone(),
                source: ephervex_core::CompatibilitySource::Bundled,
                support: std::collections::BTreeMap::new(),
                unrepresentable: std::collections::BTreeMap::new(),
            };
            apply_minecraft_event(
                &self.ui,
                Event::MinecraftPreferencePreview {
                    generation,
                    minecraft: release,
                    preview,
                },
                &self.minecraft,
                &self.sender,
            );
            slint::platform::update_timers_and_animations();
        }

        /// The row as the model currently describes it.
        fn drafted(&self) -> MinecraftPreferenceRow {
            let id = minecraft_preference_id(&self.row.id).expect("a catalog id");
            self.minecraft
                .borrow()
                .row_at(&id)
                .expect("the row stays on screen")
                .1
        }

        /// The control for this row, found by the label the row gives it.
        fn control(&self, role: i_slint_backend_testing::AccessibleRole) -> ElementHandle {
            self.control_of(&self.row, role)
        }

        fn control_of(
            &self,
            row: &MinecraftPreferenceRow,
            role: i_slint_backend_testing::AccessibleRole,
        ) -> ElementHandle {
            let mut found = ElementQuery::from_root(&self.ui)
                .match_descendants()
                .match_accessible_role(role)
                .find_all()
                .into_iter()
                .filter(|element| element.accessible_label().as_ref() == Some(&row.label));
            let control = found.next().expect("the row has its control on screen");
            // A query can report one element several times, so uniqueness is
            // judged by where the element sits.
            let at = control.absolute_position();
            assert!(
                found.all(|other| other.absolute_position() == at),
                "the control is not unique"
            );
            control
        }

        fn click(&self, control: &ElementHandle, across: f32) {
            let at = control.absolute_position();
            let size = control.size();
            let position =
                slint::LogicalPosition::new(at.x + size.width * across, at.y + size.height / 2.0);
            let window = self.ui.window();
            window.dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
            window.dispatch_event(slint::platform::WindowEvent::PointerPressed {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
            window.dispatch_event(slint::platform::WindowEvent::PointerReleased {
                position,
                button: slint::platform::PointerEventButton::Left,
            });
            slint::platform::update_timers_and_animations();
        }

        fn press(&self, key: slint::platform::Key) {
            self.press_text(SharedString::from(key));
        }

        fn press_text(&self, text: SharedString) {
            let window = self.ui.window();
            window.dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
            window.dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
            slint::platform::update_timers_and_animations();
        }

        /// Tabs until the keyboard focus ring is drawn on this control.
        ///
        /// Pressing keys on the way would change the controls Tab passes
        /// through, so the ring is what says where the focus is.
        fn tab_to(&self, control: &ElementHandle) {
            let at = control.absolute_position();
            let size = control.size();
            for _ in 0..TAB_LIMIT {
                self.press(slint::platform::Key::Tab);
                let ringed = ElementQuery::from_root(&self.ui)
                    .match_descendants()
                    .match_id("KeyboardFocus::ring")
                    .find_all()
                    .into_iter()
                    .any(|ring| {
                        let ring = ring.absolute_position();
                        ring.x >= at.x - 12.0
                            && ring.x <= at.x + size.width
                            && ring.y >= at.y - 12.0
                            && ring.y <= at.y + size.height
                    });
                if ringed {
                    return;
                }
            }
            panic!("Tab never reached the control");
        }

        /// The row with this id as the model currently describes it.
        fn drafted_row(&self, id: &SharedString) -> MinecraftPreferenceRow {
            let id = minecraft_preference_id(id).expect("a catalog id");
            self.minecraft
                .borrow()
                .row_at(&id)
                .expect("the row stays on screen")
                .1
        }

        /// Whether a key meant for the row's editor changes the row, which
        /// only happens when that editor is the one holding the focus.
        fn key_changes(&self, row: &MinecraftPreferenceRow) -> bool {
            use slint::platform::Key;

            let before = self.drafted_row(&row.id);
            match row.editor.as_str() {
                "toggle" => self.press_text(" ".into()),
                "slider" => self.press(Key::RightArrow),
                _ => {
                    self.press(Key::DownArrow);
                    self.press(Key::UpArrow);
                }
            }
            let after = self.drafted_row(&row.id);
            // A dropdown at either end moves only one way, so the pair of
            // presses can end where it started; the draft still moved.
            after.dirty != before.dirty
                || after.bool_value != before.bool_value
                || after.enum_index != before.enum_index
                || (after.number_value - before.number_value).abs() > f32::EPSILON
        }
    }

    #[test]
    fn a_second_arrow_key_on_a_slider_still_reaches_the_same_slider() {
        use i_slint_backend_testing::AccessibleRole;
        use slint::platform::Key;

        let screen = MinecraftScreen::showing("slider");
        let slider = screen.control(AccessibleRole::Slider);
        // A press on the rail focuses the slider and moves it there.
        screen.click(&slider, 0.25);
        let first = screen.drafted();
        assert!(slider.is_valid(), "the edit rebuilt the slider");

        screen.press(Key::RightArrow);
        let second = screen.drafted();
        assert!(slider.is_valid(), "the edit rebuilt the slider");
        assert!(
            (second.number_value - (first.number_value + first.number_step)).abs() < 1e-3,
            "the first arrow key stepped the slider"
        );

        screen.press(Key::RightArrow);
        let third = screen.drafted();
        assert!(slider.is_valid(), "the edit rebuilt the slider");
        assert!(
            (third.number_value - (second.number_value + second.number_step)).abs() < 1e-3,
            "the second arrow key must still reach the slider"
        );
    }

    #[test]
    fn a_second_activation_of_a_switch_still_reaches_the_same_switch() {
        use i_slint_backend_testing::AccessibleRole;

        let screen = MinecraftScreen::showing("toggle");
        let switch = screen.control(AccessibleRole::Checkbox);
        let before = screen.drafted().bool_value;

        screen.tab_to(&switch);
        screen.press_text(" ".into());
        assert_eq!(screen.drafted().bool_value, !before);
        assert!(switch.is_valid(), "the edit rebuilt the switch");

        screen.press_text(" ".into());
        assert!(switch.is_valid(), "the edit rebuilt the switch");
        assert_eq!(
            screen.drafted().bool_value,
            before,
            "the second activation must still reach the switch"
        );
        assert_eq!(switch.accessible_checked(), Some(before));
    }

    #[test]
    fn a_second_arrow_key_on_a_dropdown_still_reaches_the_same_dropdown() {
        use i_slint_backend_testing::AccessibleRole;
        use slint::platform::Key;

        let screen = MinecraftScreen::showing("enum");
        let dropdown = screen.control(AccessibleRole::Combobox);
        let start = screen.drafted().enum_index;
        let steps = i32::try_from(screen.row.enum_labels.row_count()).unwrap_or(0);
        // Each key goes the way that still has room, so a short list or an
        // end position cannot make a key a no-op.
        let toward_room = |index: i32| {
            if index + 1 < steps {
                (Key::DownArrow, index + 1)
            } else {
                (Key::UpArrow, index - 1)
            }
        };

        // A click gives the dropdown the focus; the popup it opens is closed
        // again, leaving the keyboard on the dropdown itself. Tab would pass
        // other dropdowns on the way, and an arrow key changes those too.
        screen.click(&dropdown, 0.5);
        screen.press(Key::Escape);
        let (key, expected) = toward_room(start);
        screen.press(key);
        assert_eq!(screen.drafted().enum_index, expected, "the first key");
        assert!(dropdown.is_valid(), "the edit rebuilt the dropdown");

        let (key, expected) = toward_room(expected);
        screen.press(key);
        assert!(dropdown.is_valid(), "the edit rebuilt the dropdown");
        assert_eq!(
            screen.drafted().enum_index,
            expected,
            "the second key must still reach the dropdown"
        );
    }

    #[test]
    fn a_preview_answer_after_an_edit_leaves_the_edited_control_in_place() {
        use i_slint_backend_testing::AccessibleRole;
        use slint::platform::Key;

        let screen = MinecraftScreen::showing("slider");
        let slider = screen.control(AccessibleRole::Slider);
        screen.click(&slider, 0.25);
        let first = screen.drafted();

        // The answer to the edit's preview request arrives while the user is
        // still in the slider.
        screen.answer_preview();
        assert!(slider.is_valid(), "the preview answer rebuilt the slider");

        screen.press(Key::RightArrow);
        assert!(
            (screen.drafted().number_value - (first.number_value + first.number_step)).abs() < 1e-3,
            "a key after the preview answer must still reach the slider"
        );
    }

    /// What a slider shows next to what its row's draft holds.
    fn assert_slider_matches_draft(screen: &MinecraftScreen, how: &str) {
        use i_slint_backend_testing::AccessibleRole;

        let slider = screen.control(AccessibleRole::Slider);
        let drafted = screen.drafted();
        let shown = slider
            .accessible_value()
            .and_then(|text| text.parse::<f32>().ok())
            .expect("the slider reports a number");
        assert!(
            (shown - drafted.number_value).abs() < 1e-3,
            "`{}` after {how}: the handle shows {shown} but the draft holds {}",
            drafted.id,
            drafted.number_value
        );
    }

    #[test]
    fn a_slider_shows_the_value_its_draft_holds_after_a_pointer_release() {
        use i_slint_backend_testing::AccessibleRole;

        let sliders = MinecraftScreen::slider_ids();
        assert!(!sliders.is_empty(), "the catalog has sliders");
        for id in &sliders {
            let screen = MinecraftScreen::showing_where(|rows| {
                rows.iter().position(|row| row.id.as_str() == id)
            });
            let slider = screen.control(AccessibleRole::Slider);
            for across in [0.21, 0.5, 0.77] {
                screen.click(&slider, across);
                assert_slider_matches_draft(&screen, &format!("a click at {across}"));
            }
        }
    }

    #[test]
    fn a_slider_shows_the_value_its_draft_holds_after_an_assistive_value_between_steps() {
        use i_slint_backend_testing::AccessibleRole;

        for id in &MinecraftScreen::slider_ids() {
            let screen = MinecraftScreen::showing_where(|rows| {
                rows.iter().position(|row| row.id.as_str() == id)
            });
            let row = screen.drafted();
            // Just past a step, so the draft's rounding lands on the step
            // and may leave the stored value exactly where it was.
            let between = row.number_min + row.number_step * 0.4;
            // The first edit makes the row an override, which changes more
            // than the slider; the second changes nothing but the slider.
            for _ in 0..2 {
                let slider = screen.control(AccessibleRole::Slider);
                slider.set_accessible_value(format!("{between}"));
                slint::platform::update_timers_and_animations();
                assert_slider_matches_draft(&screen, &format!("an assistive value of {between}"));
            }
        }
    }

    #[test]
    fn tabbing_out_of_a_committed_field_leaves_the_next_control_focusable() {
        use i_slint_backend_testing::AccessibleRole;
        use slint::platform::Key;

        let screen = MinecraftScreen::showing_field_before_a_keyboard_control();
        let following = screen.following.clone().expect("a row follows the field");
        let field = screen.control(AccessibleRole::TextInput);
        let before = screen.drafted().text_value;

        screen.click(&field, 0.5);
        screen.press_text("5".into());
        // Leaving the field commits it, and the commit must not recreate the
        // control Tab is moving to.
        screen.press(Key::Tab);
        assert!(field.is_valid(), "the commit rebuilt the field");
        assert_ne!(
            screen.drafted().text_value,
            before,
            "the commit reached the draft"
        );

        screen.answer_preview();
        assert!(
            screen.key_changes(&following),
            "Tab out of the field left no control to take keys"
        );
    }
}
