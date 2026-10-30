//! Starring a Minecraft release from the dialog that offers it.
//!
//! The star sits beside the version picker rather than in a settings screen, so
//! what it is bound to and what it reports are worth pinning down in a real
//! window.

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

#[test]
fn the_star_reports_the_release_the_dialog_is_offering() {
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

    let toggled: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&toggled);
    ui.on_toggle_favorite_version(move |version| record.borrow_mut().push(version.to_string()));

    ui.set_loading(false);
    ui.set_minecraft_releases(ModelRc::new(VecModel::from(
        ["1.21.1", "1.16.3"].map(SharedString::from).to_vec(),
    )));
    ui.set_create_minecraft("1.16.3".into());
    ui.set_create_minecraft_favorite(false);
    ui.set_modal("create-instance".into());
    settle(&ui);

    // An unstarred release offers to become one, and says which.
    let star = ElementHandle::find_by_accessible_label(&ui, &offer("1.16.3"))
        .next()
        .expect("the dialog offers a star");
    star.mock_single_click(PointerEventButton::Left);
    assert_eq!(toggled.borrow().as_slice(), ["1.16.3"]);

    // A starred one offers the opposite, so the control reads as one thing in
    // two states rather than as two controls.
    ui.set_create_minecraft_favorite(true);
    settle(&ui);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &offer("1.16.3"))
            .next()
            .is_none()
    );
    let starred = ElementHandle::find_by_accessible_label(&ui, &withdraw("1.16.3"))
        .next()
        .expect("a starred release offers to be unstarred");
    starred.mock_single_click(PointerEventButton::Left);
    assert_eq!(toggled.borrow().as_slice(), ["1.16.3", "1.16.3"]);
}

fn offer(version: &str) -> String {
    lookup_args("gui-favorite-version", [("value", Arg::from(version))])
}

fn withdraw(version: &str) -> String {
    lookup_args("gui-unfavorite-version", [("value", Arg::from(version))])
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn install_i18n(ui: &AppWindow) {
    let global = ui.global::<GuiI18n>();
    global.on_text(|id| ephervex_i18n::lookup(id.as_str()).into());
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
