//! The three lists on the settings screen that each hold one remembered value.
//!
//! Watched download folders, sites allowed to serve modpacks, and favorite
//! Minecraft releases are the same shape of thing: something the user told
//! Ephervex to keep, and one way to take it back. They are read down one page,
//! so they are rows of the same card, and the two that throw something away say
//! so in the same color.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString};

#[test]
fn each_watched_folder_is_listed_with_its_own_way_to_remove_it() {
    let ui = window();
    let removed: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = Rc::clone(&removed);
    ui.on_remove_watched_folder(move |folder| sink.borrow_mut().push(folder.to_string()));

    storage_settings(&ui);
    ui.set_watched_download_folders(
        ["/home/melanx/Downloads", "/tmp"]
            .map(SharedString::from)
            .as_slice()
            .into(),
    );
    settle(&ui);

    let buttons = buttons_labeled(&ui, "gui-remove");
    assert_eq!(buttons.len(), 2, "one way to remove each folder");

    buttons[0].invoke_accessible_default_action();
    settle(&ui);

    assert_eq!(
        removed.borrow().as_slice(),
        ["/home/melanx/Downloads".to_owned()],
        "removing has to name the folder that was pressed"
    );
}

#[test]
fn an_empty_watched_folder_list_says_what_is_watched_instead() {
    let ui = window();
    storage_settings(&ui);
    ui.set_watched_download_folders(ModelRc::default());
    settle(&ui);

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-watched-download-folders-empty"))
            .any(|element| element.size().width > 0.0),
        "an empty list has to explain itself rather than look broken"
    );
}

/// The rows are read down one page, so a folder that is inset differently from
/// a favorite release, or runs the width of the page while the others do not, is
/// what makes the page look unfinished.
#[test]
fn a_watched_folder_row_is_inset_and_capped_like_a_favorite_version_row() {
    let ui = window();
    storage_settings(&ui);
    ui.set_watched_download_folders(["/tmp"].map(SharedString::from).as_slice().into());
    settle(&ui);
    // Read now rather than held: a handle into a page that has been switched
    // away from reports nothing.
    let folder = shown(&ui, "/tmp").expect("the folder has a row");
    let left = folder.absolute_position().x;
    let remove = buttons_labeled(&ui, "gui-remove");
    let remove = remove.first().expect("the folder can be removed");
    let width = remove.absolute_position().x + remove.size().width - left;

    ui.set_page(4);
    ui.set_settings_tab("General".into());
    ui.set_favorite_minecraft_versions(["1.21.1"].map(SharedString::from).as_slice().into());
    settle(&ui);
    let version = shown(&ui, "1.21.1").expect("the version has a row");

    assert!(
        (left - version.absolute_position().x).abs() < 1.0,
        "a folder starts at {left}px beside a version at {}px",
        version.absolute_position().x
    );
    // The column these cards are held to, so a row does not stretch across a
    // wide window while the list under it stays put.
    assert!(
        width <= 520.0,
        "a folder row runs {width}px across a 520px column"
    );
}

/// The visible element carrying one value's text.
fn shown(ui: &AppWindow, value: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, value).find(|element| element.size().width > 0.0)
}

fn buttons_labeled(ui: &AppWindow, id: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, &lookup(id))
        .filter(|element| {
            element.size().width > 0.0 && element.accessible_role() == Some(AccessibleRole::Button)
        })
        .collect()
}

fn storage_settings(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_page(4);
    ui.set_settings_tab("Storage".into());
    // Settings pages are long, and an assertion about what is on screen has to
    // be given a screen to be on.
    ui.window().set_size(PhysicalSize::new(1480, 1600));
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
