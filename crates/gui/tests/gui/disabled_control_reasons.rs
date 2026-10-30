//! Controls that cannot be pressed say why.
//!
//! The reason a disabled control shows on hover is also its accessible
//! description, so these tests read it from there.

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

#[test]
fn a_disabled_create_button_names_the_missing_release() {
    let ui = window();
    open_create_instance(&ui, "");

    let create = control(&ui, &lookup("gui-create-instance"));
    assert_eq!(create.accessible_enabled(), Some(false));
    assert_eq!(
        create.accessible_description().as_deref(),
        Some(lookup("gui-hint-choose-minecraft-release").as_str())
    );
}

#[test]
fn a_disabled_create_button_names_the_missing_loader_version() {
    let ui = window();
    open_create_instance(&ui, "1.16.3");
    ui.set_create_loader("Fabric".into());
    ui.set_create_loader_version("".into());
    settle(&ui);

    let create = control(&ui, &lookup("gui-create-instance"));
    assert_eq!(create.accessible_enabled(), Some(false));
    assert_eq!(
        create.accessible_description().as_deref(),
        Some(lookup("gui-hint-choose-loader-version").as_str())
    );
}

#[test]
fn a_disabled_star_says_to_choose_a_release_first() {
    let ui = window();
    open_create_instance(&ui, "");

    let star = control(
        &ui,
        &lookup_args("gui-favorite-version", [("value", Arg::from(""))]),
    );
    assert_eq!(star.accessible_enabled(), Some(false));
    assert_eq!(
        star.accessible_description().as_deref(),
        Some(lookup("gui-hint-choose-minecraft-release").as_str())
    );
}

#[test]
fn a_control_that_works_has_no_reason_to_give() {
    let ui = window();
    open_create_instance(&ui, "1.16.3");

    let cancel = control(&ui, &lookup("gui-cancel"));
    assert_eq!(cancel.accessible_enabled(), Some(true));
    assert_eq!(cancel.accessible_description().as_deref(), Some(""));
}

#[test]
fn the_reason_goes_away_when_the_button_becomes_available() {
    let ui = window();
    open_create_instance(&ui, "");
    let create = control(&ui, &lookup("gui-create-instance"));
    assert_eq!(create.accessible_enabled(), Some(false));

    ui.set_create_minecraft("1.16.3".into());
    settle(&ui);

    assert_eq!(create.accessible_enabled(), Some(true));
    assert_eq!(create.accessible_description().as_deref(), Some(""));
}

#[test]
fn a_disabled_star_ignores_a_click() {
    let ui = window();
    let toggled: Rc<RefCell<usize>> = Rc::default();
    let count = Rc::clone(&toggled);
    ui.on_toggle_favorite_version(move |_| *count.borrow_mut() += 1);
    open_create_instance(&ui, "");

    control(
        &ui,
        &lookup_args("gui-favorite-version", [("value", Arg::from(""))]),
    )
    .mock_single_click(PointerEventButton::Left);

    assert_eq!(*toggled.borrow(), 0);
}

fn control(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .next()
        .expect("the window has a control with this label")
}

fn open_create_instance(ui: &AppWindow, minecraft: &str) {
    ui.set_loading(false);
    ui.set_minecraft_releases(ModelRc::new(VecModel::from(
        ["1.21.1", "1.16.3"].map(SharedString::from).to_vec(),
    )));
    ui.set_create_name("Test".into());
    ui.set_create_minecraft(minecraft.into());
    ui.set_create_minecraft_favorite(false);
    ui.set_modal("create-instance".into());
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
    settle(&ui);
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
