//! Slint callback registration and UI model updates.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use copypasta::{ClipboardContext, ClipboardProvider as _};
use ephervex_core::live_diagnostics::{LogFilter, LogLevel, RetainedLog};
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::{
    ComponentHandle as _, Image, Model as _, ModelRc, SharedString, Timer, TimerMode, VecModel,
};

use crate::backend::{
    self, AccountData, ActivityData, ArtifactCacheStatusData, Command, DiagnosticContent,
    DiagnosticData, Event, JavaData, LiveLogLine, LiveLogSource, LiveLogUpdate, ManagedModData,
    ModSearchData, PreferenceData, ServerWorkspaceData, SettingsData, Snapshot,
};
use crate::presentation::{self, parse_network, validate_ip, validate_port};
use crate::ui::{
    AccountRow, ActivityRow, AppWindow, DiagnosticRow, GuiI18n, InstanceRow, JavaRow,
    LinkedTargetRow, LogChunk, ModRow, ModSearchRow, ModVersionRow, PendingEntityRow,
    PreferenceRow, ServerRow, Theme,
};

pub struct Controller {
    _poll_timer: Timer,
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
    },
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
}

#[derive(Debug, Default)]
struct CreationTracker {
    next_id: u64,
    keys: HashSet<CreationKey>,
    pending: Vec<PendingCreation>,
}

struct LiveViewerState {
    active: Option<LiveLogSource>,
    history: RetainedLog,
    chunks: Rc<VecModel<LogChunk>>,
    filter: LogFilter,
    source_truncated: bool,
    pending_lines: usize,
    search_query: String,
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
        }
    }
}

impl LiveViewerState {
    fn activate(&mut self, source: LiveLogSource) {
        if self.active != Some(source) {
            self.active = Some(source);
            self.history.clear();
            self.source_truncated = false;
            self.pending_lines = 0;
            self.search_query.clear();
        }
    }

    fn deactivate(&mut self) {
        self.active = None;
        self.pending_lines = 0;
    }

    fn apply(&mut self, update: LiveLogUpdate) -> bool {
        if self.active != Some(update.source) {
            return false;
        }
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
            }
        }
        self.pending_lines = self.pending_lines.saturating_add(added);
        update.reset || update.missing || update.source_truncated || added > 0
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
        open_state.borrow_mut().activate(source);
        if let Some(ui) = weak.upgrade() {
            ui.set_log_query(SharedString::default());
            ui.set_log_follow(true);
            render_live_viewer(&ui, &mut open_state.borrow_mut());
        }
        send(&sender, Command::OpenLiveLog(source));
    });

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
        let mut text = record.text.clone();
        if let Some(truncation) = record.truncation {
            text.push('\n');
            text.push_str(&lookup_args(
                "gui-log-bytes-omitted",
                [("count", Arg::from(truncation.omitted_bytes))],
            ));
        }
        content_columns = content_columns.max(
            record
                .text
                .lines()
                .map(|line| line.chars().count())
                .max()
                .unwrap_or(0),
        );
        push_record_chunks(&mut chunks, text, record.level, CHUNK_BYTES);
    }
    LogPresentation {
        chunks,
        content_columns,
        match_count,
    }
}

fn push_record_chunks(
    chunks: &mut Vec<LogChunk>,
    mut text: String,
    level: LogLevel,
    max_bytes: usize,
) {
    while text.len() > max_bytes {
        let mut split = max_bytes;
        while !text.is_char_boundary(split) {
            split -= 1;
        }
        if let Some(newline) = text[..split].rfind('\n') {
            split = newline;
        }
        if split == 0 {
            split = max_bytes.min(text.len());
            while !text.is_char_boundary(split) {
                split -= 1;
            }
        }
        let mut remainder = text.split_off(split);
        chunks.push(LogChunk {
            text: text.into(),
            level: level.as_str().into(),
        });
        if remainder.starts_with('\n') {
            remainder.remove(0);
        }
        text = remainder;
    }
    chunks.push(LogChunk {
        text: text.into(),
        level: level.as_str().into(),
    });
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
    ui.set_log_content_columns(to_i32(presentation.content_columns.min(10_000)));
    ui.set_log_trace_enabled(state.filter.is_enabled(LogLevel::Trace));
    ui.set_log_debug_enabled(state.filter.is_enabled(LogLevel::Debug));
    ui.set_log_info_enabled(state.filter.is_enabled(LogLevel::Info));
    ui.set_log_warn_enabled(state.filter.is_enabled(LogLevel::Warn));
    ui.set_log_error_enabled(state.filter.is_enabled(LogLevel::Error));
    ui.set_log_fatal_enabled(state.filter.is_enabled(LogLevel::Fatal));
    ui.set_log_other_enabled(state.filter.is_enabled(LogLevel::Other));
    if chunks_changed {
        ui.set_log_revision(ui.get_log_revision().saturating_add(1));
    }
    state.pending_lines = 0;
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

impl std::fmt::Debug for Controller {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Controller").finish_non_exhaustive()
    }
}

pub fn attach(ui: &AppWindow) -> Controller {
    let backend = backend::spawn();
    let receiver = Rc::new(RefCell::new(backend.events));
    let activities = Rc::new(RefCell::new(Vec::<ActivityData>::new()));
    let creations = Rc::new(RefCell::new(CreationTracker::default()));
    let live_viewer = Rc::new(RefCell::new(LiveViewerState::default()));
    register_i18n(ui);
    register_toasts(ui);
    register_callbacks(ui, &backend.commands, Rc::clone(&creations), &live_viewer);

    let weak = ui.as_weak();
    let poll_receiver = Rc::clone(&receiver);
    let poll_activities = Rc::clone(&activities);
    let poll_creations = Rc::clone(&creations);
    let poll_live_viewer = Rc::clone(&live_viewer);
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
        let mut live_dirty = false;
        for event in poll_receiver.borrow_mut().try_iter() {
            match event {
                Event::LiveLog(update) => {
                    live_dirty |= poll_live_viewer.borrow_mut().apply(update);
                }
                other => apply_event(&ui, other, &poll_activities, &poll_creations),
            }
        }
        if live_dirty {
            if ui.get_log_follow() {
                render_live_viewer(&ui, &mut poll_live_viewer.borrow_mut());
            } else {
                ui.set_log_pending_lines(to_i32(poll_live_viewer.borrow().pending_lines));
            }
        }
    });

    Controller {
        _poll_timer: poll_timer,
    }
}

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

fn register_i18n(ui: &AppWindow) {
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

fn register_callbacks(
    ui: &AppWindow,
    commands: &Sender<Command>,
    creations: Rc<RefCell<CreationTracker>>,
    live_viewer: &Rc<RefCell<LiveViewerState>>,
) {
    register_catalog_actions(ui, commands);
    register_client_actions(ui, commands, Rc::clone(&creations));

    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_submit_create_server(
        move |name, client, minecraft, loader, loader_version, persistent, automatic_port, port| {
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
            };
            let pending = pending_creation(&key);
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
                },
            );
        },
    );

    let sender = commands.clone();
    ui.on_select_server(move |_| send(&sender, Command::Refresh));

    register_server_actions(ui, commands);
    register_diagnostics(ui, commands);
    register_live_viewer(ui, commands, live_viewer);
    register_settings(ui, commands);

    let weak = ui.as_weak();
    ui.on_copy_text(move |text| {
        if let Some(ui) = weak.upgrade() {
            copy_to_clipboard(&ui, text.into());
        }
    });

    let sender = commands.clone();
    ui.on_cancel_operation(move |id| send(&sender, Command::CancelOperation(id.into())));
}

fn copy_to_clipboard(ui: &AppWindow, text: String) {
    let copied = ClipboardContext::new()
        .and_then(|mut clipboard| clipboard.set_contents(text))
        .is_ok();
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

fn register_client_actions(
    ui: &AppWindow,
    commands: &Sender<Command>,
    creations: Rc<RefCell<CreationTracker>>,
) {
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
        let pending = pending_creation(&key);
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

    let sender = commands.clone();
    ui.on_choose_import_file(move || send(&sender, Command::PickImport));

    let sender = commands.clone();
    ui.on_submit_import(move |archive, name, persistent| {
        send(
            &sender,
            Command::ImportPack {
                archive: PathBuf::from(archive.as_str()),
                name: name.into(),
                persistent,
            },
        );
    });

    register_instance_selection(ui, commands, creations);

    let sender = commands.clone();
    ui.on_launch_client(move |id| {
        if let Some(id) = backend::parse_instance_id(&id) {
            send(&sender, Command::LaunchClient(id));
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
            send(&sender, Command::DuplicateClient(id));
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
    ui.on_load_instance_workspace(move |id| {
        if let Some(id) = backend::parse_instance_id(&id) {
            send(&sender, Command::LoadInstanceWorkspace(id));
        }
    });

    register_mod_actions(ui, commands);
}

fn register_mod_actions(ui: &AppWindow, commands: &Sender<Command>) {
    register_mod_mutations(ui, commands);
    register_mod_browser(ui, commands);

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

/// Finding a mod in the catalog and reviewing what installing it would involve.
///
/// Both handlers do the same two things and nothing else: turn a display id back
/// into a typed one, and send a command. Searching, deciding what fits this
/// instance, and resolving a dependency graph all happen in `ephervex-core`;
/// this side never asks a provider anything.
fn register_mod_browser(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    let handle = ui.as_weak();
    ui.on_search_mods(move |instance, query| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(instance) = backend::parse_instance_id(&instance) else {
            return;
        };
        // What was submitted, which is not what the field says: the user may
        // keep typing while the catalog answers, and those results are still
        // the ones they asked for.
        ui.set_mod_search_submitted(query.clone());
        send(
            &sender,
            Command::SearchMods {
                instance,
                query: query.to_string(),
            },
        );
    });

    let sender = commands.clone();
    let handle = ui.as_weak();
    ui.on_review_mod(move |instance, project| {
        let Some(ui) = handle.upgrade() else { return };
        let Some(instance) = backend::parse_instance_id(&instance) else {
            return;
        };
        ui.set_mod_plan_requested(project.clone());
        // Reviewing is planning: the confirmation it opens is the same one the
        // linked-server section lives in, so a review already answers "and what
        // would this do to my servers".
        send(
            &sender,
            Command::PlanMod {
                instance,
                project: project.to_string(),
            },
        );
    });
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
    ui.on_install_mod(move |instance, project, version| {
        let Some(ui) = handle.upgrade() else { return };
        if let Some(instance) = backend::parse_instance_id(&instance) {
            send(
                &sender,
                Command::InstallMod {
                    instance,
                    project: project.to_string(),
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
    ui.on_select_instance(move |id| {
        if let Some(ui) = weak.upgrade() {
            let linked = ui
                .get_servers()
                .iter()
                .filter(|row| row.linked_client_id == id)
                .collect::<Vec<_>>();
            ui.set_linked_servers(model(linked));
            apply_pending_creations(&ui, &creations.borrow());
        }
        send(&sender, Command::Refresh);
    });
}

fn register_server_actions(ui: &AppWindow, commands: &Sender<Command>) {
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

    let sender = commands.clone();
    ui.on_server_start(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::StartServer(id));
        }
    });

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
    ui.on_save_storage_settings(move |root| {
        send(&sender, Command::SaveStorageRoot(root.into()));
    });
    register_artifact_cache(ui, commands);
    let sender = commands.clone();
    ui.on_choose_storage_root(move || send(&sender, Command::PickStorageRoot));
    let sender = commands.clone();
    ui.on_reset_storage_root(move || send(&sender, Command::SaveStorageRoot(String::new())));

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

    let sender = commands.clone();
    ui.on_save_preference(move |key, value| {
        if let Some(key) = ephervex_core::PreferenceKey::parse(&key) {
            send(
                &sender,
                Command::SetPreference {
                    key,
                    value: value.into(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_reset_preference(move |key| {
        if let Some(key) = ephervex_core::PreferenceKey::parse(&key) {
            send(&sender, Command::ResetPreference(key));
        }
    });
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

fn apply_event(
    ui: &AppWindow,
    event: Event,
    activities: &Rc<RefCell<Vec<ActivityData>>>,
    creations: &Rc<RefCell<CreationTracker>>,
) {
    match event {
        Event::Snapshot(snapshot) => {
            apply_snapshot(ui, snapshot);
            apply_pending_creations(ui, &creations.borrow());
        }
        Event::ArtifactCacheStatus(status) => apply_artifact_cache_status(ui, status),
        Event::MinecraftReleases(versions) => apply_minecraft_releases(ui, versions),
        Event::LoaderVersions {
            form,
            loader,
            minecraft,
            versions,
        } => apply_loader_versions(ui, &form, loader, &minecraft, &versions),
        Event::ManagedMods(data) => {
            // A late answer for an instance the user has already navigated
            // away from is discarded rather than rendered under the wrong
            // name. The workspace it belongs to already showed its durable
            // state, and reopening it asks again.
            if presentation::result_is_current(&data.instance, &ui.get_selected_instance_id()) {
                ui.set_managed_mods(model(managed_mod_rows(data.mods)));
                ui.set_managed_mods_notice(data.notice.into());
            }
        }
        Event::ModVersions(data) => apply_mod_versions(ui, data),
        Event::ModRemovalPlan(data) => apply_mod_removal_plan(ui, data),
        Event::ModResults(results) => apply_mod_results(ui, results),
        Event::ModPlan(plan) => apply_mod_install_plan(ui, plan),
        Event::LinkedApply(section) => apply_linked_confirmation(ui, section),
        Event::LinkedResult(result) => apply_linked_result(ui, result),
        Event::ServerWorkspace(workspace) => apply_server_workspace(ui, workspace),
        Event::LiveLog(_) => {}
        Event::Diagnostics {
            rows,
            scope,
            scope_id,
            kind,
        } => {
            ui.set_diagnostics(model(diagnostic_rows(rows)));
            ui.set_selected_diagnostic_scope(scope.into());
            ui.set_selected_diagnostic_id(scope_id.into());
            ui.set_selected_diagnostic_kind(kind.into());
            ui.set_selected_diagnostic_name(SharedString::default());
            ui.set_diagnostic_content(SharedString::default());
            ui.set_diagnostic_path(SharedString::default());
            ui.set_diagnostic_title(SharedString::default());
        }
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
            ui.set_mod_action_pending(false);
        }
        Event::CreationFinished { request_id } => {
            creations.borrow_mut().finish(&request_id);
            apply_pending_creations(ui, &creations.borrow());
        }
        Event::ImportAnalysis {
            path,
            suggested_name,
            summary,
        } => {
            ui.set_import_path(path.display().to_string().into());
            if ui.get_import_name().is_empty() {
                ui.set_import_name(suggested_name.into());
            }
            ui.set_import_analysis(summary.into());
            ui.set_modal_error(SharedString::default());
        }
        Event::StorageRootPicked(path) => {
            ui.set_persistent_root(path.display().to_string().into());
        }
    }
}

fn apply_minecraft_releases(ui: &AppWindow, versions: Vec<String>) {
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
    } else {
        ui.set_create_loader_versions(rows);
        ui.set_create_loader_version(versions.first().cloned().unwrap_or_default().into());
    }
}

fn pending_creation(key: &CreationKey) -> PendingCreation {
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
        linked_client_id: match key {
            CreationKey::Server { client, .. } => client.clone(),
            CreationKey::Client { .. } => String::new(),
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
                // which the typed resolution treats as a custom size — so the
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

fn apply_snapshot(ui: &AppWindow, snapshot: Snapshot) {
    let Snapshot {
        instances,
        servers,
        accounts,
        preferences,
        settings,
        eula,
    } = snapshot;
    ui.set_show_linked_servers(settings.show_linked_servers);
    ui.set_make_default_account_operator(settings.make_default_account_operator);
    ui.set_operator_permission_level(i32::from(settings.operator_permission_level));
    apply_instance_snapshot(ui, instances);
    apply_server_snapshot(ui, servers);
    ui.set_accounts(model(account_rows(accounts)));
    ui.set_preferences(model(preference_rows(preferences)));
    apply_artifact_cache_settings(ui, &settings);
    ui.set_storage_root(settings.storage_root.into());
    ui.set_persistent_root(settings.persistent_root.into());
    ui.set_default_persistent_root(settings.default_persistent_root.into());
    ui.set_persistent_root_is_custom(settings.persistent_root_origin.is_override());
    ui.set_microsoft_client_id(settings.microsoft_client_id.into());
    ui.set_effective_microsoft_client_id(settings.effective_microsoft_client_id.into());
    ui.set_microsoft_client_id_is_custom(settings.microsoft_client_id_origin.is_override());
    ui.set_user_agent(settings.user_agent.into());
    ui.set_effective_user_agent(settings.effective_user_agent.into());
    ui.set_user_agent_is_custom(settings.user_agent_origin.is_override());
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
    ui.set_instances(model(instance_rows));
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
    ui.set_servers(model(server_rows));
    ui.set_global_servers(model(global_rows));
    ui.set_linked_servers(model(linked_rows));
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

fn preference_rows(rows: Vec<PreferenceData>) -> Vec<PreferenceRow> {
    rows.into_iter()
        .map(|row| {
            let numeric_value = row.value.parse::<f32>().unwrap_or_default();
            PreferenceRow {
                key: row.key.into(),
                label: row.label.into(),
                value: row.value.into(),
                customized: row.customized,
                kind: row.kind.into(),
                syntax: row.syntax.into(),
                numeric_value,
            }
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
    if !data.title.is_empty() {
        ui.set_mod_action_name(data.title.into());
    }
    ui.set_mod_versions(model(mod_version_rows(data.versions)));
}

/// Shows the hits of the search the user is currently waiting on.
///
/// An answer is discarded when it is for another instance, and when it is for a
/// query that has since been superseded — two searches run on their own threads
/// and can finish in either order, so without this a slow older one could
/// replace the newer results already on screen.
fn apply_mod_results(ui: &AppWindow, results: backend::ModResultsData) {
    if !presentation::result_is_current(&results.instance, &ui.get_selected_instance_id()) {
        return;
    }
    if results.query != ui.get_mod_search_submitted().as_str() {
        return;
    }
    ui.set_mod_results(model(mod_search_rows(results.rows)));
}

/// Shows what installing a mod would involve, here and on each linked server.
fn apply_mod_install_plan(ui: &AppWindow, plan: backend::ModPlanData) {
    if !presentation::result_is_current(&plan.instance, &ui.get_selected_instance_id()) {
        return;
    }
    // A confirmation arriving over a modal the user opened while it was being
    // planned would replace what they are looking at, and would leave the
    // linked-server rows describing a different operation than the one on
    // screen.
    let modal = ui.get_modal();
    if !modal.is_empty() && modal != "mod-plan" {
        return;
    }
    // Two reviews can be in flight; the older one must not replace the newer.
    if plan.requested != ui.get_mod_plan_requested().as_str() {
        return;
    }
    ui.set_mod_plan_instance(plan.instance.into());
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
    ui.set_linked_result_title(result.title.into());
    ui.set_linked_result_summary(result.summary.into());
    ui.set_linked_results(model(linked_target_rows(result.rows)));
    ui.set_mod_action_pending(false);
    ui.set_modal_error(SharedString::default());
    ui.set_modal("mod-result".into());
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
            restriction: row.restriction.into(),
            title: row.title.into(),
            file_name: row.file_name.into(),
            version: row.version.into(),
            version_known: row.version_known,
            provider: row.provider.into(),
            origin: row.origin.into(),
            origin_kind: row.origin_kind.into(),
            compatibility: row.compatibility.into(),
            author: row.author.into(),
            icon: cached_image(&row.icon),
            missing: row.missing,
            pack_owned: row.pack_owned,
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
            downloads: row.downloads.into(),
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
    use crate::presentation::ActivityPresentation;
    use ephervex_core::ArtifactCacheLimit;
    use ephervex_i18n::has;

    use super::*;

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
    fn duplicate_creation_is_rejected_until_the_original_request_finishes() {
        let key = CreationKey::Client {
            name: "Rendering Repro".to_owned(),
            minecraft: "1.20.1".to_owned(),
            loader: "Forge".to_owned(),
            loader_version: "47.3.0".to_owned(),
            persistent: false,
        };
        let mut tracker = CreationTracker::default();
        let request = tracker.accept(key.clone(), pending_creation(&key));
        assert!(request.is_some());
        let request = request.unwrap_or_default();

        assert!(
            tracker
                .accept(key.clone(), pending_creation(&key))
                .is_none()
        );
        assert_eq!(tracker.pending.len(), 1);

        tracker.finish(&request);
        assert!(
            tracker
                .accept(key.clone(), pending_creation(&key))
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
        };
        let mut second = first.clone();
        if let CreationKey::Server { name, .. } = &mut second {
            *name = "Second Server".to_owned();
        }
        let mut tracker = CreationTracker::default();

        assert!(
            tracker
                .accept(first.clone(), pending_creation(&first))
                .is_some()
        );
        assert!(
            tracker
                .accept(second.clone(), pending_creation(&second))
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
            }
        };
        let first = normalize("1.20.1", "Forge", "47.3.0");
        let same_request = normalize("26.2", "NeoForge", "21.0.0");
        let mut tracker = CreationTracker::default();

        assert!(
            tracker
                .accept(first.clone(), pending_creation(&first))
                .is_some()
        );
        assert!(
            tracker
                .accept(same_request.clone(), pending_creation(&same_request))
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

    #[test]
    fn card_workspace_actions_select_the_target_before_opening_it() {
        let source = include_str!("ui.rs");
        assert!(source.contains(
            "open-workspace(id) => { root.selected-instance-id = id; root.selected-instance-index = index; root.select-instance(id); root.workspace = \"instance\";"
        ));
        assert!(source.contains(
            "open-workspace(id) => { root.selected-global-server-id = id; root.selected-global-server-index = index; root.selected-server-id = id; root.server-workspace-return-client = \"\"; root.select-server(id); root.workspace = \"server\";"
        ));
        assert!(source.contains(
            "card-area := TouchArea { width: parent.width - 250px; clicked => { root.choose(item.id); } double-clicked => { root.open-workspace(item.id); } }"
        ));
    }

    #[test]
    fn minecraft_preferences_customize_on_edit_and_reset_without_a_management_toggle() {
        let source = include_str!("ui.rs");
        let reset = "gui-reset";
        assert!(source.contains(&format!(
            r#"if item.customized : AppButton {{ text: GuiI18n.text("{reset}")"#
        )));
        assert!(source.contains(
            "Toggle { checked: item.value == \"true\"; toggled(value) => { root.save(item.key"
        ));
        assert!(!source.contains("checked: item.enabled"));
        assert!(!source.contains("enabled: item.enabled"));
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
        assert!(source.contains("component PreferenceCard inherits Rectangle"));
        assert!(source.contains("for pref in root.preferences : PreferenceCard { item: pref;"));
        assert!(source.contains("ScrollView { viewport-width: self.width;"));
    }

    #[test]
    fn console_and_diagnostic_text_use_native_read_only_selection() {
        let source = include_str!("ui.rs");
        assert!(source.contains("accessible-read-only: true"));
        assert!(source.contains("read-only: true"));
        assert!(source.contains("selectable-input.copy()"));
        assert!(source.contains("selectable-input.select-all()"));
        assert!(source.contains("component LiveLogViewer inherits Rectangle"));
        assert!(source.contains("chunk-input.copy()"));
        assert!(source.contains("chunk-input.select-all()"));
        assert!(source.contains("root.copy-live-log()"));
        assert!(source.contains(
            "SelectableText { text: root.diagnostic-content; font-size: root.technical-text-size * 1px; }"
        ));
    }

    #[test]
    fn large_live_history_uses_bounded_production_filter_and_chunk_updates() {
        let source = LiveLogSource::Client(ephervex_model::InstanceId::new());
        let mut state = LiveViewerState::default();
        state.activate(source);

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
            assert!(state.apply(LiveLogUpdate {
                source,
                reset: batch == 0,
                missing: false,
                source_truncated: false,
                lines,
            }));
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
    }

    #[test]
    fn fov_presentation_keeps_core_degree_semantics() {
        let rows = preference_rows(vec![PreferenceData {
            key: "fov".to_owned(),
            label: "Field of View".to_owned(),
            value: "70".to_owned(),
            customized: true,
            kind: "slider".to_owned(),
            syntax: "field of view in degrees, 30 to 110".to_owned(),
        }]);

        assert!((rows[0].numeric_value - 70.0).abs() < f32::EPSILON);
        assert_eq!(rows[0].value, "70");
    }
}
