//! The dialog that asks the user to fetch a release themselves.
//!
//! It exists because a CurseForge project's owner can forbid other launchers
//! from downloading their files. That is a step in a version change, not a
//! dead end, so the dialog has to offer the provider's own page for the exact
//! file and a way out that changes nothing.

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, BlockedFileRow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

const FILE_PAGE: &str = "https://www.curseforge.com/minecraft/mc-mods/repro/download/7951520";

#[test]
fn the_dialog_offers_the_providers_own_page_for_the_exact_file() {
    let ui = window();
    let opened: Rc<RefCell<Vec<String>>> = Rc::default();
    let recorder = Rc::clone(&opened);
    ui.on_open_blocked_file(move |url| recorder.borrow_mut().push(url.to_string()));
    acquisition(&ui, &[blocked(FILE_PAGE)]);

    let open = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-open-in-browser"))
        .find(|element| element.size().width > 0.0)
        .expect("the dialog offers a way to fetch the file");
    open.mock_single_click(PointerEventButton::Left);

    assert_eq!(
        opened.borrow().as_slice(),
        [FILE_PAGE.to_owned()],
        "the page the provider publishes for this file, never an assembled address"
    );
}

/// The row still names the project and the file when no page can be built for
/// it, because the user can find it themselves from a name and cannot from a
/// blank row.
#[test]
fn a_file_with_no_page_still_names_itself() {
    let ui = window();
    acquisition(&ui, &[blocked("")]);

    assert!(
        ElementHandle::find_by_accessible_label(&ui, "restricted-1.0.0.jar")
            .any(|element| element.size().width > 0.0),
        "the file's own name has to be on screen to be found by hand"
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-open-in-browser"))
            .all(|element| !element.accessible_enabled().unwrap_or(false)),
        "nothing offers a page that could not be built"
    );
}

/// Leaving is always available and always harmless: nothing has been written
/// by the time this dialog is shown.
#[test]
fn leaving_the_dialog_stops_the_wait() {
    let ui = window();
    let canceled: Rc<RefCell<Vec<String>>> = Rc::default();
    let recorder = Rc::clone(&canceled);
    ui.on_cancel_mod_acquisition(move |id| recorder.borrow_mut().push(id.to_string()));
    acquisition(&ui, &[blocked(FILE_PAGE)]);

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-cancel"))
        .find(|element| element.size().width > 0.0)
        .expect("the dialog can be left")
        .mock_single_click(PointerEventButton::Left);

    assert_eq!(
        canceled.borrow().as_slice(),
        ["42".to_owned()],
        "the folder watcher this prompt belongs to has to be stopped"
    );
    assert_eq!(ui.get_modal().as_str(), "");
}

fn blocked(browser_url: &str) -> BlockedFileRow {
    BlockedFileRow {
        identity: "curseforge:391382:7951520".into(),
        project_line: "Repro Mod".into(),
        file_line: "restricted-1.0.0.jar".into(),
        browser_url: browser_url.into(),
    }
}

fn acquisition(ui: &AppWindow, rows: &[BlockedFileRow]) {
    ui.set_loading(false);
    ui.set_mod_acquisition_id("42".into());
    ui.set_mod_acquisition_name("Repro Mod".into());
    ui.set_mod_acquisition_items(ModelRc::new(VecModel::from(rows.to_vec())));
    ui.set_modal("mod-acquire".into());
    settle(ui);
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
        lookup_args(
            id.as_str(),
            [("value", Arg::Text(value.to_string().into()))],
        )
        .into()
    });
}
