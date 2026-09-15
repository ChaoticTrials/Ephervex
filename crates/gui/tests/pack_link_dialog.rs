//! Agreeing to a modpack a web page offered.
//!
//! The dialog is the whole safety story of the feature: a link can name any
//! address, and what makes downloading from it legitimate is that a person was
//! shown the address and said yes (ADR 0027). So what it shows, and what it
//! sends back, are both worth pinning down.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::{ComponentHandle, PhysicalSize};

/// The address as the backend hands it over: the real one to fetch, and the
/// trimmed one to show.
const ADDRESS: &str =
    "https://dumps.chaotictrials.de/api/dump/5ca16367/modpack?platform=modrinth&token=s3cret";
const SHOWN: &str = "https://dumps.chaotictrials.de/api/dump/5ca16367/modpack";

#[test]
fn the_dialog_shows_the_address_being_agreed_to() {
    let ui = window();
    consent_dialog(&ui);

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-open-pack-link-detail"))
            .any(|element| element.size().width > 0.0),
        "the dialog has to say what it is asking about"
    );
}

/// A link's query is where identifiers and tokens live. A prompt that printed
/// one would be teaching people to skim past secrets, and the question being
/// asked is which machine Ephervex will talk to.
#[test]
fn the_address_shown_carries_no_query_or_token() {
    let ui = window();
    consent_dialog(&ui);

    assert_eq!(ui.get_pack_link_display(), SHOWN);
    assert!(!ui.get_pack_link_display().contains("s3cret"));
    // The address that would actually be fetched is unchanged.
    assert_eq!(ui.get_pack_link_url(), ADDRESS);
}

#[test]
fn confirming_sends_the_address_back_with_the_answer() {
    let ui = window();
    let answered = record_answers(&ui);
    consent_dialog(&ui);

    confirm(&ui);

    assert_eq!(
        answered.borrow().as_slice(),
        [(ADDRESS.to_owned(), true)],
        "the address has to go back whole, with the remembering answer"
    );
}

/// Remembering is the default, because a second import from the same site
/// should be one click. Unticking it is a deliberate act and has to survive.
#[test]
fn declining_to_remember_still_downloads_but_records_nothing() {
    let ui = window();
    let answered = record_answers(&ui);
    consent_dialog(&ui);

    remember_switch(&ui)
        .expect("the dialog offers to stop asking")
        .invoke_accessible_default_action();
    settle(&ui);
    confirm(&ui);

    assert_eq!(answered.borrow().as_slice(), [(ADDRESS.to_owned(), false)]);
}

#[test]
fn cancelling_sends_nothing_at_all() {
    let ui = window();
    let answered = record_answers(&ui);
    consent_dialog(&ui);

    widest(&ui, &lookup("gui-cancel"))
        .expect("the dialog can be declined")
        .invoke_accessible_default_action();
    settle(&ui);

    assert!(
        answered.borrow().is_empty(),
        "declining must not reach the backend"
    );
    assert_eq!(ui.get_modal(), "", "declining closes the dialog");
}

/// Whether the scheme is claimed at all is settled by installing (ADR 0028,
/// ADR 0029). The control that used to stand here meant "yes" on an AppImage
/// and nothing at all on a packaged install, and had three lines of copy
/// explaining that.
#[test]
fn settings_offers_no_switch_for_the_link_handler() {
    let ui = window();
    general_settings(&ui);

    let sites = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-trusted-pack-hosts"))
        .find(|element| element.size().height > 0.0)
        .expect("the section lists the sites agreed to");
    let favorites = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-favorite-versions"))
        .find(|element| element.size().height > 0.0)
        .expect("the next section follows the sites list");
    let top = sites.absolute_position().y;
    let bottom = favorites.absolute_position().y;

    assert!(
        !ElementHandle::find_by_element_type_name(&ui, "Toggle").any(|element| {
            element.size().width > 0.0
                && element.absolute_position().y >= top
                && element.absolute_position().y < bottom
        }),
        "the sites-allowed section has to hold no control for turning link handling on or off"
    );
}

/// Granting happens in the link dialog, so settings is the only place an answer
/// can be seen and taken back.
#[test]
fn settings_lists_each_trusted_site_and_offers_to_forget_it() {
    let ui = window();
    let forgotten: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = Rc::clone(&forgotten);
    ui.on_forget_pack_host(move |host| sink.borrow_mut().push(host.to_string()));

    general_settings(&ui);
    ui.set_trusted_pack_hosts(
        ["dumps.chaotictrials.de", "packs.example.com"]
            .map(slint::SharedString::from)
            .as_slice()
            .into(),
    );
    settle(&ui);

    let buttons: Vec<ElementHandle> =
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-forget-pack-host"))
            .filter(|element| {
                element.size().width > 0.0
                    && element.accessible_role() == Some(AccessibleRole::Button)
            })
            .collect();
    assert_eq!(buttons.len(), 2, "one way to forget each site");

    buttons[0].invoke_accessible_default_action();
    settle(&ui);

    assert_eq!(
        forgotten.borrow().as_slice(),
        ["dumps.chaotictrials.de".to_owned()],
        "forgetting has to name the site that was pressed"
    );
}

#[test]
fn settings_says_so_when_no_site_has_been_agreed_to() {
    let ui = window();
    general_settings(&ui);
    ui.set_trusted_pack_hosts(slint::ModelRc::default());
    settle(&ui);

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-no-trusted-pack-hosts"))
            .any(|element| element.size().width > 0.0),
        "an empty list has to explain itself rather than look broken"
    );
}

fn general_settings(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_page(4);
    ui.set_workspace_tab("General".into());
    // Tall enough that the whole section is laid out. General settings is a
    // long page, and an assertion about what is on screen has to be given a
    // screen to be on.
    ui.window().set_size(PhysicalSize::new(1480, 1600));
    settle(ui);
}

/// The dialog as the backend opens it.
fn consent_dialog(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_page(0);
    ui.set_pack_link_url(ADDRESS.into());
    ui.set_pack_link_display(SHOWN.into());
    ui.set_pack_link_remember(true);
    ui.set_modal("open-pack-link".into());
    settle(ui);
}

fn confirm(ui: &AppWindow) {
    widest(ui, &lookup("gui-open-pack-link-confirm"))
        .expect("the dialog offers a way to go ahead")
        .invoke_accessible_default_action();
    settle(ui);
}

fn remember_switch(ui: &AppWindow) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, &lookup("gui-open-pack-link-remember"))
        .find(|element| element.size().width > 0.0)
}

fn record_answers(ui: &AppWindow) -> Rc<RefCell<Vec<(String, bool)>>> {
    let answered: Rc<RefCell<Vec<(String, bool)>>> = Rc::default();
    let sink = Rc::clone(&answered);
    ui.on_open_pack_link(move |url, remember| {
        sink.borrow_mut().push((url.to_string(), remember));
    });
    answered
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
