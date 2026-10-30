//! Where a published server archive can be chosen, and what carries it back.
//!
//! Two dialogs offer the choice: create-server, and import when it is also
//! building a linked server. In both the archive path lives in a property
//! rather than in a field the button reads, so a wiring mistake would build an
//! ordinary server and say nothing.

use crate::support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow, ServerPackRules};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::WindowEvent;
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, SharedString, VecModel};

/// A standalone server can be built from an archive, so the dialog offers the
/// choice with no client selected.
#[test]
fn the_dialog_offers_an_archive_for_a_standalone_server() {
    let ui = window();
    let chosen = Rc::new(RefCell::new(0_u32));
    let sink = Rc::clone(&chosen);
    ui.on_choose_server_pack(move || *sink.borrow_mut() += 1);

    open_create_server(&ui, "");
    let browse =
        widest(&ui, &lookup("gui-choose-server-pack")).expect("the dialog offers a file chooser");
    browse.invoke_accessible_default_action();

    assert_eq!(*chosen.borrow(), 1);
}

/// A linked server can be built from one too: the client decides the exact
/// Minecraft and loader selection, and the archive decides the content.
#[test]
fn the_dialog_offers_an_archive_for_a_linked_server_as_well() {
    let ui = window();
    open_create_server(&ui, "repro-client");

    assert!(
        widest(&ui, &lookup("gui-choose-server-pack")).is_some(),
        "a linked server takes its selection from the client, not its content"
    );
}

/// The chosen path is what reaches the backend, unchanged.
#[test]
fn submitting_carries_the_chosen_archive_path() {
    let ui = window();
    let submitted: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&submitted);
    ui.on_submit_create_server(move |_, _, _, _, _, _, _, _, server_pack, _| {
        sink.borrow_mut().push(server_pack.to_string());
    });

    open_create_server(&ui, "repro-client");
    ui.set_create_server_name("Repro Server".into());
    ui.set_create_server_pack("/tmp/Repro_server_pack_1.2.3.zip".into());
    ui.set_create_server_pack_name("Repro_server_pack_1.2.3.zip".into());
    settle(&ui);

    let create = widest(&ui, &lookup("gui-create-server")).expect("the dialog can be submitted");
    create.invoke_accessible_default_action();

    assert_eq!(
        submitted.borrow().as_slice(),
        ["/tmp/Repro_server_pack_1.2.3.zip".to_owned()]
    );
}

/// A server with no archive submits an empty path rather than a stale one.
#[test]
fn a_server_built_the_usual_way_submits_no_archive() {
    let ui = window();
    let submitted: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&submitted);
    ui.on_submit_create_server(move |_, _, _, _, _, _, _, _, server_pack, _| {
        sink.borrow_mut().push(server_pack.to_string());
    });

    open_create_server(&ui, "repro-client");
    ui.set_create_server_name("Repro Server".into());
    settle(&ui);

    let create = widest(&ui, &lookup("gui-create-server")).expect("the dialog can be submitted");
    create.invoke_accessible_default_action();

    assert_eq!(submitted.borrow().as_slice(), [String::new()]);
}

/// Clearing the choice puts the dialog back where it started, so a mistaken
/// pick does not become the server's content.
#[test]
fn clearing_the_choice_removes_the_archive_from_the_submission() {
    let ui = window();
    let weak = ui.as_weak();
    ui.on_clear_server_pack(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_create_server_pack(SharedString::default());
            ui.set_create_server_pack_name(SharedString::default());
        }
    });

    open_create_server(&ui, "");
    ui.set_create_server_pack("/tmp/Repro_server_pack_1.2.3.zip".into());
    ui.set_create_server_pack_name("Repro_server_pack_1.2.3.zip".into());
    settle(&ui);

    let clear =
        widest(&ui, &lookup("gui-clear-server-pack")).expect("a chosen archive can be unchosen");
    clear.invoke_accessible_default_action();
    settle(&ui);

    assert_eq!(ui.get_create_server_pack(), SharedString::default());
    assert_eq!(ui.get_create_server_pack_name(), SharedString::default());
}

/// An untouched dialog creates the server its placeholder names.
#[test]
fn an_untouched_dialog_submits_the_suggested_name() {
    let ui = window();
    let submitted: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&submitted);
    ui.on_submit_create_server(move |name, _, _, _, _, _, _, _, _, _| {
        sink.borrow_mut().push(name.to_string());
    });

    open_create_server(&ui, "repro-client");
    ui.set_create_server_name(SharedString::default());
    ui.set_create_server_name_suggestion("Caveopolis 2 Server".into());
    settle(&ui);

    let create = button(&ui, &lookup("gui-create-server")).expect("the dialog can be submitted");
    create.invoke_accessible_default_action();

    assert_eq!(
        submitted.borrow().as_slice(),
        ["Caveopolis 2 Server".to_owned()]
    );
}

/// A typed name still wins over the suggestion behind it.
#[test]
fn a_typed_name_is_submitted_instead_of_the_suggestion() {
    let ui = window();
    let submitted: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&submitted);
    ui.on_submit_create_server(move |name, _, _, _, _, _, _, _, _, _| {
        sink.borrow_mut().push(name.to_string());
    });

    open_create_server(&ui, "repro-client");
    ui.set_create_server_name_suggestion("Caveopolis 2 Server".into());
    ui.set_create_server_name("Bisect A".into());
    settle(&ui);

    let create = button(&ui, &lookup("gui-create-server")).expect("the dialog can be submitted");
    create.invoke_accessible_default_action();

    assert_eq!(submitted.borrow().as_slice(), ["Bisect A".to_owned()]);
}

/// The import dialog only asks for server files once a server is wanted.
#[test]
fn the_import_dialog_offers_an_archive_only_when_it_is_building_a_server() {
    let ui = window();

    open_import(&ui, false);
    assert!(
        widest(&ui, &lookup("gui-choose-server-pack")).is_none(),
        "an import with no server has nothing to put server files in"
    );

    open_import(&ui, true);
    assert!(
        widest(&ui, &lookup("gui-choose-server-pack")).is_some(),
        "a pack's own server files are the content of the server it builds"
    );
}

/// The import dialog carries its own choice, not the create-server dialog's.
#[test]
fn submitting_an_import_carries_its_own_archive() {
    let ui = window();
    let submitted: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&submitted);
    ui.on_submit_import(move |_, _, _, server_pack| {
        sink.borrow_mut().push(server_pack.to_string());
    });

    open_import(&ui, true);
    ui.set_create_server_pack("/tmp/wrong_server_pack.zip".into());
    ui.set_import_server_pack("/tmp/Repro_server_pack_1.2.3.zip".into());
    ui.set_import_server_pack_name("Repro_server_pack_1.2.3.zip".into());
    ui.set_import_ready(true);
    settle(&ui);

    let import = button(&ui, &lookup("gui-import-modpack")).expect("the dialog can be submitted");
    import.invoke_accessible_default_action();

    assert_eq!(
        submitted.borrow().as_slice(),
        ["/tmp/Repro_server_pack_1.2.3.zip".to_owned()]
    );
}

/// What the rule was asked: the selection's loader and build, then what the
/// server files name.
type RuleQuestion = (String, String, String, String, bool);

/// Server files that contradict a standalone selection keep Create unavailable
/// and say why, and the rule is asked about exactly what the dialog holds.
#[test]
fn a_standalone_selection_its_server_files_contradict_cannot_be_created() {
    let ui = window();
    let asked: Rc<RefCell<Vec<RuleQuestion>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&asked);
    ui.global::<ServerPackRules>().on_conflict(
        move |_, loader, version, _, pack_loader, pack_version, mods| {
            sink.borrow_mut().push((
                loader.to_string(),
                version.to_string(),
                pack_loader.to_string(),
                pack_version.to_string(),
                mods,
            ));
            if loader == pack_loader && version == pack_version {
                SharedString::default()
            } else {
                "These server files were made for NeoForge, not Vanilla.".into()
            }
        },
    );

    open_standalone_with_server_files(&ui);
    let create = button(&ui, &lookup("gui-create-server")).expect("the dialog has a Create");
    assert_eq!(create.accessible_enabled(), Some(false));
    assert_eq!(
        create.accessible_description().as_deref(),
        Some("These server files were made for NeoForge, not Vanilla.")
    );
    assert_eq!(
        asked.borrow().last(),
        Some(&(
            "Vanilla".to_owned(),
            String::new(),
            "NeoForge".to_owned(),
            "21.1.77".to_owned(),
            true
        ))
    );

    ui.set_create_server_loader("NeoForge".into());
    ui.set_create_server_loader_version("21.1.77".into());
    settle(&ui);
    let create = button(&ui, &lookup("gui-create-server")).expect("the dialog has a Create");
    assert_eq!(create.accessible_enabled(), Some(true));
}

/// A linked server's selection is its client's, so the dialog leaves the
/// comparison to creation instead of blocking on a selection it does not show.
#[test]
fn a_linked_server_is_not_held_back_by_the_standalone_rule() {
    let ui = window();
    ui.global::<ServerPackRules>()
        .on_conflict(|_, _, _, _, _, _, _| "always in conflict".into());

    open_create_server(&ui, "repro-client");
    ui.set_create_server_name("Repro Server".into());
    ui.set_create_server_pack("/tmp/ServerFiles-4.2.zip".into());
    ui.set_create_server_pack_name("ServerFiles-4.2.zip".into());
    ui.set_create_server_pack_mods(true);
    settle(&ui);

    let create = button(&ui, &lookup("gui-create-server")).expect("the dialog has a Create");
    assert_eq!(create.accessible_enabled(), Some(true));
}

/// The note saying where filled-in values came from shows with its archive
/// and goes with it.
#[test]
fn the_server_files_note_shows_only_while_its_archive_is_chosen() {
    let ui = window();
    let note = "Minecraft 1.21.1 with NeoForge 21.1.77, as named by the server files.";

    open_standalone_with_server_files(&ui);
    ui.set_create_server_pack_note(note.into());
    settle(&ui);
    assert!(widest(&ui, note).is_some());

    ui.set_create_server_pack(SharedString::default());
    ui.set_create_server_pack_name(SharedString::default());
    settle(&ui);
    assert!(widest(&ui, note).is_none());
}

fn open_standalone_with_server_files(ui: &AppWindow) {
    open_create_server(ui, "");
    ui.set_create_server_name("Repro Server".into());
    ui.set_minecraft_releases(ModelRc::new(VecModel::from(vec![SharedString::from(
        "1.21.1",
    )])));
    ui.set_create_server_minecraft("1.21.1".into());
    ui.set_create_server_loader("Vanilla".into());
    ui.set_create_server_loader_version(SharedString::default());
    ui.set_create_server_pack("/tmp/ServerFiles-4.2.zip".into());
    ui.set_create_server_pack_name("ServerFiles-4.2.zip".into());
    ui.set_create_server_pack_mods(true);
    ui.set_create_server_pack_minecraft("1.21.1".into());
    ui.set_create_server_pack_loader("NeoForge".into());
    ui.set_create_server_pack_loader_version("21.1.77".into());
    settle(ui);
}

/// With a linked server wanted and its archive chosen, the server files row
/// sits above the dialog's buttons rather than drawn over them, whether the
/// window leaves the dialog its full height or makes it scroll.
#[test]
fn the_import_dialog_keeps_its_server_files_row_above_its_buttons() {
    let ui = window();
    for height in [900_u16, 560] {
        ui.window()
            .set_size(PhysicalSize::new(1480, u32::from(height)));
        open_import(&ui, true);
        ui.set_import_server_pack("/tmp/Repro_server_pack_1.2.3.zip".into());
        ui.set_import_server_pack_name("Repro_server_pack_1.2.3.zip".into());
        settle(&ui);
        // Scrolled to its end, so a short window shows the row at all.
        let detail = widest(&ui, &lookup("gui-import-detail")).expect("the dialog explains itself");
        ui.window().dispatch_event(WindowEvent::PointerScrolled {
            position: LogicalPosition::new(
                detail.absolute_position().x + detail.size().width / 2.0,
                detail.absolute_position().y + detail.size().height / 2.0,
            ),
            delta_x: 0.0,
            delta_y: -2000.0,
        });
        settle(&ui);

        let browse = widest(&ui, &lookup("gui-choose-server-pack"))
            .expect("the dialog offers a file chooser");
        let cancel = button(&ui, &lookup("gui-cancel")).expect("the dialog can be canceled");
        let import =
            button(&ui, &lookup("gui-import-modpack")).expect("the dialog can be submitted");

        let browse_bottom = browse.absolute_position().y + browse.size().height;
        assert!(
            browse_bottom <= cancel.absolute_position().y,
            "at window height {height} the server files row ends at {browse_bottom}, below the \
             top of Cancel at {}",
            cancel.absolute_position().y
        );
        for action in [&cancel, &import] {
            let bottom = action.absolute_position().y + action.size().height;
            assert!(
                bottom <= f32::from(height),
                "at window height {height} a dialog button ends at {bottom}, off the window"
            );
        }
    }
}

/// A long error wraps onto several lines without reaching the buttons below it.
#[test]
fn a_long_import_error_stays_clear_of_the_buttons() {
    let ui = window();
    support::set_minimum_width(&ui, 900);
    open_import(&ui, true);
    let error = "The pack could not be read because one of the files it lists is missing \
                 from the archive and no provider offers a download for it either";
    ui.set_modal_error(error.into());
    settle(&ui);

    let message = ElementHandle::find_by_accessible_label(&ui, error)
        .find(|element| element.size().width > 0.0)
        .expect("the dialog shows the error");
    let cancel = button(&ui, &lookup("gui-cancel")).expect("the dialog can be canceled");

    let message_bottom = message.absolute_position().y + message.size().height;
    assert!(
        message_bottom <= cancel.absolute_position().y,
        "the error ends at {message_bottom}, below the top of Cancel at {}",
        cancel.absolute_position().y
    );
}

/// A pack summary that wraps keeps the rows after it below its card, and keeps
/// the card's own padding below its last line, whatever the font's metrics.
#[test]
fn a_long_pack_summary_stays_clear_of_the_rows_below_it() {
    let ui = window();
    support::set_minimum_width(&ui, 900);
    open_import(&ui, false);
    ui.set_import_pack_header("Pack".into());
    ui.set_import_minecraft_line("Minecraft 1.20.1".into());
    ui.set_import_loader_line("NeoForge 47.1.106".into());
    ui.set_import_required_line("Required files: 412".into());
    ui.set_import_optional_line("Optional files: 37".into());
    settle(&ui);
    let (short_bottom, short_next) =
        summary_and_next_row(&ui, "Required files: 412 | Optional files: 37");

    let counts = "Required files: 412 | Optional files: 37, none of them selected yet, \
                  and each one can be chosen below before the pack is imported";
    ui.set_import_pack_header("A Very Long Modpack Name For Testing Wrapping 1.2.3".into());
    ui.set_import_author_line("by Someone, Someone Else, A Third Author and Their Team".into());
    ui.set_import_optional_line(
        "Optional files: 37, none of them selected yet, and each one can be chosen below \
         before the pack is imported"
            .into(),
    );
    settle(&ui);
    let (summary_bottom, next_row) = summary_and_next_row(&ui, counts);

    assert!(
        summary_bottom <= next_row,
        "the pack summary ends at {summary_bottom}, below the next row at {next_row}"
    );
    assert!(
        next_row - summary_bottom >= short_next - short_bottom,
        "the wrapped summary leaves {} below its last line, less than the {} a one-line summary leaves",
        next_row - summary_bottom,
        short_next - short_bottom
    );
}

/// Where the summary's counts line ends, and where the instance name row starts.
fn summary_and_next_row(ui: &AppWindow, counts: &str) -> (f32, f32) {
    let summary = ElementHandle::find_by_accessible_label(ui, counts)
        .find(|element| element.size().width > 0.0)
        .expect("the dialog summarizes the pack");
    let name_label =
        ElementHandle::find_by_accessible_label(ui, &lookup("gui-instance-name-optional"))
            .find(|element| element.size().width > 0.0)
            .expect("the dialog asks for an instance name");
    (
        summary.absolute_position().y + summary.size().height,
        name_label.absolute_position().y,
    )
}

fn open_import(ui: &AppWindow, create_server: bool) {
    // Over the settings with one instance listed: Home and the empty list each
    // have an "Import Modpack" button with the dialog's label, which would be
    // found instead of the dialog's own.
    support::open_settings(ui, "General");
    ui.set_instances(ModelRc::new(VecModel::from(vec![InstanceRow {
        id: "listed".into(),
        name: "Listed".into(),
        ..InstanceRow::default()
    }])));
    ui.set_import_create_server(create_server);
    ui.set_import_server_pack(SharedString::default());
    ui.set_import_server_pack_name(SharedString::default());
    ui.set_modal("import".into());
    settle(ui);
}

fn open_create_server(ui: &AppWindow, client: &str) {
    ui.set_loading(false);
    ui.set_create_server_client_id(client.into());
    ui.set_create_server_name_suggestion(SharedString::default());
    ui.set_create_server_pack(SharedString::default());
    ui.set_create_server_pack_name(SharedString::default());
    ui.set_modal("create-server".into());
    settle(ui);
}

/// The widest *button* with this label.
///
/// A heading can carry the same words as the button beneath it, and only one of
/// the two has an action to invoke.
fn button(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label)
        .filter(|element| element.accessible_enabled().is_some())
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .filter(|element| element.size().width > 0.0)
}

fn widest(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label)
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .filter(|element| element.size().width > 0.0)
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

#[test]
fn a_server_for_an_instance_is_titled_as_a_linked_server() {
    let ui = window();
    ui.set_create_server_client_id("client".into());
    ui.set_modal("create-server".into());
    settle(&ui);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-create-linked-server"))
            .any(|element| element.size().width > 0.0),
        "the dialog says it creates a linked server"
    );

    ui.set_create_server_client_id("".into());
    settle(&ui);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-modal-new-server"))
            .any(|element| element.size().width > 0.0),
        "a standalone server keeps its own title"
    );
}

/// An empty port field means the port is chosen on creation, and the account
/// check chosen in the dialog is what is submitted.
#[test]
fn an_empty_port_is_automatic_and_the_account_check_is_submitted() {
    let ui = window();
    let submitted: Rc<RefCell<Vec<(bool, String, bool)>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&submitted);
    ui.on_submit_create_server(move |_, _, _, _, _, _, automatic, port, _, online| {
        sink.borrow_mut()
            .push((automatic, port.to_string(), online));
    });

    open_create_server(&ui, "repro-client");
    ui.set_create_server_name("Repro Server".into());
    settle(&ui);
    widest(&ui, &lookup("gui-accounts-online"))
        .expect("the account choice")
        .invoke_accessible_default_action();
    widest(&ui, &lookup("gui-create-server"))
        .expect("the dialog can be submitted")
        .invoke_accessible_default_action();

    assert_eq!(submitted.borrow().as_slice(), [(true, String::new(), true)]);
}
