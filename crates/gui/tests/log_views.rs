//! What the log views offer, and where each control leads.
//!
//! All three findings here are markup: a control that only exists while there
//! is somewhere to scroll to, chips that decide which of three readings of a
//! log is shown, and a Start that has to leave the user looking at the output.
//! None of it is visible from the callbacks alone, so the window is built and
//! clicked for real.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, DiagnosticRow, GuiI18n, LogChunk, ServerRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, SharedString, VecModel};

#[test]
fn a_log_scrolled_up_offers_a_way_back_to_the_bottom() {
    let ui = window();
    let followed: Rc<RefCell<Vec<bool>>> = Rc::default();
    let record = Rc::clone(&followed);
    ui.on_live_log_follow_changed(move |follow| record.borrow_mut().push(follow));

    server_logs(&ui);
    // Far more lines than fit, so the view has somewhere to go, and following
    // off, which is the state someone who scrolled up is in.
    ui.set_log_chunks(chunks(400));
    ui.set_log_has_content(true);
    ui.set_log_follow(false);
    settle(&ui);

    let jump = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-jump-latest"))
        .find(|element| element.size().width > 0.0)
        .expect("a log with more lines than fit offers a way to its end");
    jump.mock_single_click(PointerEventButton::Left);
    settle(&ui);

    assert!(
        ui.get_log_follow(),
        "arriving at the end must resume following, or the next line scrolls away again"
    );
    assert_eq!(followed.borrow().as_slice(), [true]);
}

#[test]
fn a_log_that_fits_offers_nothing_to_scroll_to() {
    let ui = window();
    server_logs(&ui);
    ui.set_log_chunks(chunks(2));
    ui.set_log_has_content(true);
    settle(&ui);

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-jump-latest"))
            .all(|element| element.size().width == 0.0),
        "a view already showing everything must not offer a button that does nothing"
    );
}

/// The three ways one entity's logs are read, on one tab.
#[test]
fn a_servers_logs_offer_the_live_log_saved_files_and_crash_reports() {
    let ui = window();
    let opened: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&opened);
    ui.on_open_log_file(move |_, _, name| record.borrow_mut().push(name.to_string()));
    let listed: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&listed);
    ui.on_load_diagnostics(move |_, _, kind| record.borrow_mut().push(kind.to_string()));

    server_logs(&ui);
    settle(&ui);

    for chip in ["gui-live-latest-log", "gui-log-files", "gui-crash-reports"] {
        assert!(
            ElementHandle::find_by_accessible_label(&ui, &lookup(chip))
                .next()
                .is_some(),
            "{chip} is not offered on a server's Logs tab"
        );
    }

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-log-files"))
        .next()
        .expect("the saved logs chip")
        .mock_single_click(PointerEventButton::Left);
    assert!(ui.get_log_shows_files());
    assert_eq!(listed.borrow().as_slice(), ["log"]);

    ui.set_diagnostics(ModelRc::new(VecModel::from(vec![log_file()])));
    settle(&ui);
    ElementHandle::find_by_accessible_label(&ui, "2026-08-30-1.log.gz")
        .next()
        .expect("the list holds the file")
        .mock_single_click(PointerEventButton::Left);

    assert_eq!(
        opened.borrow().as_slice(),
        ["2026-08-30-1.log.gz"],
        "a saved log has to go through the filterable view, not a plain document"
    );
    assert_eq!(ui.get_selected_diagnostic_name(), "2026-08-30-1.log.gz");
}

/// A saved log is read through the same viewer, so it has the same filters.
#[test]
fn a_saved_log_is_filterable_like_the_live_one() {
    let ui = window();
    let levels: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&levels);
    ui.on_set_log_level(move |level, _| record.borrow_mut().push(level.to_string()));

    server_logs(&ui);
    ui.set_log_shows_files(true);
    ui.set_selected_diagnostic_name("2026-08-30-1.log.gz".into());
    ui.set_diagnostics(ModelRc::new(VecModel::from(vec![log_file()])));
    ui.set_log_chunks(chunks(6));
    ui.set_log_has_content(true);
    settle(&ui);

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-filter-error"))
        .next()
        .expect("a saved log has the level filters the live one has")
        .mock_single_click(PointerEventButton::Left);

    assert_eq!(levels.borrow().as_slice(), ["error"]);
}

/// A drag that leaves the row it began on has to reach the rows below it.
///
/// This is the arithmetic that replaces a text item's own selection, which can
/// never cross the element it belongs to: with every row one line of the same
/// height, the row under the pointer is the pressed row plus a count of row
/// heights. Only a rendered window can say whether that holds.
#[test]
fn a_drag_from_one_console_line_reaches_the_lines_below_it() {
    let ui = window();
    let points: Rc<RefCell<Vec<(i32, i32, bool)>>> = Rc::default();
    let record = Rc::clone(&points);
    ui.on_select_log_at(move |row, column, extend| {
        record.borrow_mut().push((row, column, extend));
    });

    server_console(&ui);
    settle(&ui);

    let rows = console_rows(&ui);
    assert!(
        rows.len() > 4,
        "the console has to have rows to drag across"
    );
    drag(&ui, &rows[1], &rows[4]);

    let points = points.borrow();
    let (first_row, _, extend) = *points.first().expect("the press selects a point");
    assert_eq!(first_row, 1, "the press lands on the row it was made on");
    assert!(!extend, "a press without shift starts a new selection");

    let (last_row, _, extend) = *points.last().expect("the drag moves the other end");
    assert_eq!(
        last_row, 4,
        "a drag three rows down must reach three rows down"
    );
    assert!(extend, "a drag moves one end and leaves the anchor alone");
}

/// The press has to reach the row's own area rather than the text item, which
/// would otherwise begin a selection that can never leave one line.
#[test]
fn pressing_a_console_line_selects_at_the_column_under_the_pointer() {
    let ui = window();
    let points: Rc<RefCell<Vec<(i32, i32, bool)>>> = Rc::default();
    let record = Rc::clone(&points);
    ui.on_select_log_at(move |row, column, extend| {
        record.borrow_mut().push((row, column, extend));
    });

    server_console(&ui);
    settle(&ui);

    let rows = console_rows(&ui);
    let row = &rows[2];
    // Well inside the line, so the column is a real one rather than clamped.
    let position = LogicalPosition::new(
        row.absolute_position().x + 120.0,
        row.absolute_position().y + row.size().height / 2.0,
    );
    press(&ui, position);
    release(&ui, position);

    let (index, column, _) = *points.borrow().first().expect("the press is reported");
    assert_eq!(index, 2);
    assert!(
        column > 0,
        "a press inside a line must name a column, not the start of it"
    );
}

/// The keyboard half of the console's selection. Whether a shortcut reaches the
/// handler depends on something holding focus, which only a real window can say.
#[test]
fn ctrl_c_and_ctrl_a_reach_the_console() {
    let ui = window();
    let copied: Rc<RefCell<usize>> = Rc::default();
    let record = Rc::clone(&copied);
    ui.on_copy_log_selection(move || *record.borrow_mut() += 1);
    let all: Rc<RefCell<usize>> = Rc::default();
    let record = Rc::clone(&all);
    ui.on_select_all_log(move || *record.borrow_mut() += 1);

    server_console(&ui);
    settle(&ui);

    // Pressing inside the log is what gives the console the keyboard, exactly
    // as it does for a user.
    let rows = console_rows(&ui);
    press(&ui, center(&rows[1]));
    release(&ui, center(&rows[1]));
    settle(&ui);

    hold_control(&ui);
    key(&ui, "c");
    assert_eq!(*copied.borrow(), 1, "Ctrl+C copies the selection");
    key(&ui, "a");
    assert_eq!(*all.borrow(), 1, "Ctrl+A selects everything shown");
    release_control(&ui);

    // Without the modifier the same letters are ordinary text and must not act.
    key(&ui, "c");
    assert_eq!(*copied.borrow(), 1, "an unmodified c is not a shortcut");
}

/// A double click selects the word under it, which is how an id gets copied.
#[test]
fn double_clicking_a_console_line_selects_the_word_under_it() {
    let ui = window();
    let words: Rc<RefCell<Vec<(i32, i32)>>> = Rc::default();
    let record = Rc::clone(&words);
    ui.on_select_log_word_at(move |row, column| record.borrow_mut().push((row, column)));

    server_console(&ui);
    settle(&ui);

    let rows = console_rows(&ui);
    let position = LogicalPosition::new(
        rows[2].absolute_position().x + 120.0,
        rows[2].absolute_position().y + rows[2].size().height / 2.0,
    );
    for _ in 0..2 {
        press(&ui, position);
        release(&ui, position);
    }

    let words = words.borrow();
    let (row, column) = *words.last().expect("the double click is reported");
    assert_eq!(row, 2);
    assert!(
        column > 0,
        "the word is taken at the column under the pointer"
    );
}

fn hold_control(ui: &AppWindow) {
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Control.into(),
    });
}

fn release_control(ui: &AppWindow) {
    ui.window().dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Control.into(),
    });
}

fn key(ui: &AppWindow, text: &str) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: text.into() });
}

/// The console's rows, in order.
fn console_rows(ui: &AppWindow) -> Vec<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, &lookup("gui-log-output-block")).collect()
}

fn drag(ui: &AppWindow, from: &ElementHandle, to: &ElementHandle) {
    press(ui, center(from));
    ui.window().dispatch_event(WindowEvent::PointerMoved {
        position: center(to),
    });
    release(ui, center(to));
}

fn center(element: &ElementHandle) -> LogicalPosition {
    LogicalPosition::new(
        element.absolute_position().x + element.size().width / 2.0,
        element.absolute_position().y + element.size().height / 2.0,
    )
}

fn press(ui: &AppWindow, position: LogicalPosition) {
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
}

fn release(ui: &AppWindow, position: LogicalPosition) {
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

/// The server workspace, on its Console tab, with output to select.
fn server_console(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_page(1);
    ui.set_workspace("server".into());
    ui.set_workspace_tab("Console".into());
    ui.set_servers(ModelRc::new(VecModel::from(vec![server()])));
    ui.set_selected_server_index(0);
    ui.set_selected_server_id("repro-server".into());
    ui.set_log_chunks(chunks(40));
    ui.set_log_has_content(true);
}

/// The server workspace, on its Logs tab.
fn server_logs(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_page(1);
    ui.set_workspace("server".into());
    ui.set_workspace_tab("Logs".into());
    ui.set_selected_diagnostic_kind("log".into());
    ui.set_servers(ModelRc::new(VecModel::from(vec![server()])));
    ui.set_selected_server_index(0);
    ui.set_selected_server_id("repro-server".into());
}

fn chunks(count: usize) -> ModelRc<LogChunk> {
    ModelRc::new(VecModel::from(
        (0..count)
            .map(|index| LogChunk {
                text: format!("[12:00:{index:02}] [Server thread/INFO]: line {index}").into(),
                level: "info".into(),
            })
            .collect::<Vec<_>>(),
    ))
}

fn log_file() -> DiagnosticRow {
    DiagnosticRow {
        scope_kind: "server".into(),
        scope_id: "repro-server".into(),
        entity: "Repro Server".into(),
        kind: "log".into(),
        name: "2026-08-30-1.log.gz".into(),
        path: "/tmp/ephervex/servers/repro-server/server/logs/2026-08-30-1.log.gz".into(),
        modified: "Yesterday".into(),
        size: "18 KiB".into(),
        summary: SharedString::default(),
    }
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn window() -> AppWindow {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");
    let ui = AppWindow::new().expect("window");
    install_i18n(&ui);
    ui.window().set_size(PhysicalSize::new(1480, 900));
    ui.show().expect("show");
    ui
}

fn install_i18n(ui: &AppWindow) {
    let global = ui.global::<GuiI18n>();
    global.on_text(|id| lookup(id.as_str()).into());
    global.on_count(|id, count| {
        lookup_args(id.as_str(), [("count", Arg::Count(i64::from(count)))]).into()
    });
    global.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.to_string()))]).into()
    });
    global.on_values(|id, first, second| {
        lookup_args(
            id.as_str(),
            [
                ("first", Arg::from(first.to_string())),
                ("second", Arg::from(second.to_string())),
            ],
        )
        .into()
    });
}

fn server() -> ServerRow {
    ServerRow {
        id: "repro-server".into(),
        name: "Repro Server".into(),
        minecraft: "1.21.1".into(),
        loader: "NeoForge 21.1.249".into(),
        loader_kind: "neoforge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        relationship: "Standalone".into(),
        linked_client_id: SharedString::default(),
        state: "Stopped".into(),
        state_kind: "stopped".into(),
        state_detail: "Not running".into(),
        address: "127.0.0.1:25565".into(),
        path: "/tmp/ephervex/servers/repro-server".into(),
        installed: true,
        can_start: true,
        can_stop: false,
        can_restart: false,
        can_kill: false,
        can_send_command: false,
        can_connect: false,
        connect_reason: SharedString::default(),
        needs_attention: false,
    }
}
