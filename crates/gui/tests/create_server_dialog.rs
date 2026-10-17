//! Where a published server archive can be chosen, and what carries it back.
//!
//! Two dialogs offer the choice: create-server, and import when it is also
//! building a linked server. In both the archive path lives in a property
//! rather than in a field the button reads, so a wiring mistake would build an
//! ordinary server and say nothing.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, ServerPackRules};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

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
    ui.on_submit_create_server(move |_, _, _, _, _, _, _, _, server_pack| {
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
    ui.on_submit_create_server(move |_, _, _, _, _, _, _, _, server_pack| {
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
    ui.on_submit_create_server(move |name, _, _, _, _, _, _, _, _| {
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
    ui.on_submit_create_server(move |name, _, _, _, _, _, _, _, _| {
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

fn open_import(ui: &AppWindow, create_server: bool) {
    ui.set_loading(false);
    // Away from the instances page, whose own "Import Modpack" button carries
    // the same label as the dialog's confirm and would be found instead.
    ui.set_page(1);
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
