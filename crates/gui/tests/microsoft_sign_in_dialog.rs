//! The dialog that holds a Microsoft sign-in's code while the sign-in waits.
//!
//! It is the only place the code can be read, copied and the wait ended, so it
//! has to show all of that at every text size, and say the code to a screen
//! reader.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, Theme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, PhysicalSize};

const CODE: &str = "ABCD1234";
const ADDRESS: &str = "https://microsoft.com/devicelogin";

const WINDOW_HEIGHT: u16 = 900;

/// Padding between the dialog's edge and its content.
const DIALOG_PADDING: f32 = 22.0;

#[test]
fn the_dialog_fits_all_of_its_content_at_every_text_size() {
    let ui = window();
    for scale in [0.9, 1.0, 1.1, 1.25] {
        ui.global::<Theme>().set_text_scale(scale);
        sign_in_dialog(&ui);

        let close = widest(&ui, &lookup("gui-close")).expect("the dialog can be closed");
        let cancel = widest(&ui, &lookup("gui-cancel")).expect("the wait can be ended");
        let waiting = widest(&ui, &lookup("gui-microsoft-sign-in-waiting"))
            .expect("the dialog says what it waits for");
        let window_height = f32::from(WINDOW_HEIGHT);
        let card_top = close.absolute_position().y - DIALOG_PADDING;
        let card_bottom = window_height - card_top;

        assert!(
            cancel.absolute_position().y + cancel.size().height
                <= card_bottom - DIALOG_PADDING + 0.5,
            "the Cancel row has to stay inside the dialog at text scale {scale}"
        );
        assert!(
            waiting.absolute_position().y + waiting.size().height
                <= cancel.absolute_position().y + 0.5,
            "the waiting line must not run into the Cancel row at text scale {scale}"
        );
    }
}

#[test]
fn a_screen_reader_is_told_the_code_itself() {
    let ui = window();
    sign_in_dialog(&ui);

    let spoken = format!("{}: {CODE}", lookup("gui-microsoft-sign-in-code-label"));
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &spoken)
            .any(|element| element.size().width > 0.0),
        "the code is what the dialog exists to communicate"
    );
}

#[test]
fn copying_sends_the_code_and_opening_asks_for_the_page() {
    let ui = window();
    let copied: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = Rc::clone(&copied);
    ui.on_copy_text(move |text| sink.borrow_mut().push(text.to_string()));
    let opened = Rc::new(RefCell::new(0));
    let counter = Rc::clone(&opened);
    ui.on_open_microsoft_sign_in_page(move || *counter.borrow_mut() += 1);
    sign_in_dialog(&ui);

    widest(&ui, &lookup("gui-microsoft-sign-in-copy-code"))
        .expect("the code can be copied")
        .invoke_accessible_default_action();
    widest(&ui, &lookup("gui-microsoft-sign-in-open-page"))
        .expect("the page can be opened")
        .invoke_accessible_default_action();

    assert_eq!(copied.borrow().as_slice(), [CODE.to_owned()]);
    assert_eq!(*opened.borrow(), 1);
}

#[test]
fn an_address_that_is_not_openable_leaves_the_page_button_disabled() {
    let ui = window();
    let opened = Rc::new(RefCell::new(0));
    let counter = Rc::clone(&opened);
    ui.on_open_microsoft_sign_in_page(move || *counter.borrow_mut() += 1);
    sign_in_dialog(&ui);
    ui.set_microsoft_sign_in_address_openable(false);
    settle(&ui);

    widest(&ui, &lookup("gui-microsoft-sign-in-open-page"))
        .expect("the button is still there")
        .invoke_accessible_default_action();

    assert_eq!(*opened.borrow(), 0, "a disabled button must not act");
}

#[test]
fn canceling_stops_the_sign_in_and_closes_the_dialog() {
    let ui = window();
    let canceled = Rc::new(RefCell::new(0));
    let counter = Rc::clone(&canceled);
    ui.on_cancel_microsoft_sign_in(move || *counter.borrow_mut() += 1);
    sign_in_dialog(&ui);

    widest(&ui, &lookup("gui-cancel"))
        .expect("the wait can be ended")
        .invoke_accessible_default_action();
    settle(&ui);

    assert_eq!(*canceled.borrow(), 1, "cancel has to reach the backend");
    assert_eq!(ui.get_modal(), "");
}

#[test]
fn closing_the_dialog_stops_the_sign_in_too() {
    let ui = window();
    let canceled = Rc::new(RefCell::new(0));
    let counter = Rc::clone(&canceled);
    ui.on_cancel_microsoft_sign_in(move || *counter.borrow_mut() += 1);
    sign_in_dialog(&ui);

    widest(&ui, &lookup("gui-close"))
        .expect("the dialog can be closed")
        .invoke_accessible_default_action();
    settle(&ui);

    assert_eq!(
        *canceled.borrow(),
        1,
        "a closed dialog must not leave a wait behind"
    );
    assert_eq!(ui.get_modal(), "");
}

/// The dialog as the controller opens it.
fn sign_in_dialog(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_microsoft_sign_in_code(CODE.into());
    ui.set_microsoft_sign_in_address(ADDRESS.into());
    ui.set_microsoft_sign_in_address_openable(true);
    ui.set_modal("microsoft-sign-in".into());
    settle(ui);
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
    ui.window()
        .set_size(PhysicalSize::new(1480, u32::from(WINDOW_HEIGHT)));
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
