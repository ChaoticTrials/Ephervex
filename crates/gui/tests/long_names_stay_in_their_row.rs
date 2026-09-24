//! What a long instance or server name does to the layout around it.
//!
//! A name is user-authored and has no length anybody can rely on, while the
//! columns beside it are fixed. The failure is quiet: a `Rectangle` does not
//! clip, so a caption that does not fit is drawn straight through the border it
//! belongs to instead of being cut off where anyone would notice it in review.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions, mock_elapsed_time,
};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

/// The inspector is 360px wide whatever the window is.
const INSPECTOR_WIDTH: f32 = 360.0;

const LONG_NAME: &str =
    "Garden of Glass (Questbook Edition) 1.16.5 Forge Reproduction for the Crash on World Load";
const LONG_SERVER: &str = "Garden of Glass (Questbook Edition) Development Server";

/// The only caption in the application that carries a user-authored name.
#[test]
fn a_connect_button_stays_inside_the_panel_that_holds_it() {
    let ui = window();
    instances(&ui, &[connectable()]);

    let button = widest(
        &ui,
        &lookup_args(
            "gui-launch-connect",
            [("value", Arg::from(LONG_SERVER.to_owned()))],
        ),
    )
    .expect("a linked client offers to launch and connect");

    assert!(
        button.size().width <= INSPECTOR_WIDTH,
        "the button is {}px inside a {INSPECTOR_WIDTH}px panel",
        button.size().width
    );
}

/// The card's own actions have a fixed column, so a title that asks for more
/// than the rest either pushes them off the card or is drawn over them.
#[test]
fn a_card_title_stays_inside_the_card() {
    let ui = window();
    instances(&ui, &[connectable()]);

    let card = widest(&ui, LONG_NAME).expect("the instance has a card");
    let right = card.absolute_position().x + card.size().width;
    let title = card_title(&ui);

    assert!(
        title.absolute_position().x + title.size().width <= right,
        "the title reaches {}px on a card that ends at {right}px",
        title.absolute_position().x + title.size().width
    );
}

/// Eliding keeps the layout honest and takes the text away. What is cut has to
/// stay reachable, and hovering the thing that was cut is where a person looks.
#[test]
fn a_shortened_card_title_says_the_whole_name_on_hover() {
    let ui = window();
    instances(&ui, &[connectable()]);
    let before = bearing(&ui, LONG_NAME);

    card_title(&ui).mock_single_click(PointerEventButton::Left);
    mock_elapsed_time(std::time::Duration::from_millis(600));
    settle(&ui);

    assert_eq!(
        bearing(&ui, LONG_NAME),
        before + 1,
        "hovering a cut title has to put the whole name on screen"
    );
}

/// The title owns a touch area so that it can be hovered, which puts it over
/// the card's own. A press that lands there still has to reach the card.
#[test]
fn a_press_on_the_title_still_selects_the_card() {
    let ui = window();
    let chosen: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = Rc::clone(&chosen);
    ui.on_select_instance(move |id| sink.borrow_mut().push(id.to_string()));
    instances(&ui, &[connectable()]);

    card_title(&ui).mock_single_click(PointerEventButton::Left);
    settle(&ui);

    assert_eq!(
        chosen.borrow().as_slice(),
        ["repro-client".to_owned()],
        "the strip of card under its own title has to keep selecting it"
    );
}

/// The same for the one caption that carries a name: what the button would do
/// is unreadable once the name it names has been cut out of it.
#[test]
fn a_shortened_connect_button_says_its_whole_caption_on_hover() {
    let ui = window();
    instances(&ui, &[connectable()]);
    let caption = lookup_args(
        "gui-launch-connect",
        [("value", Arg::from(LONG_SERVER.to_owned()))],
    );
    let before = bearing(&ui, &caption);

    widest(&ui, &caption)
        .expect("a linked client offers to launch and connect")
        .mock_single_click(PointerEventButton::Left);
    mock_elapsed_time(std::time::Duration::from_millis(600));
    settle(&ui);

    assert_eq!(
        bearing(&ui, &caption),
        before + 1,
        "hovering a cut caption has to put the whole caption on screen"
    );
}

/// How many things on screen carry this text. A note that opens is one more.
fn bearing(ui: &AppWindow, text: &str) -> usize {
    ElementHandle::find_by_accessible_label(ui, text)
        .filter(|element| element.size().width > 0.0)
        .count()
}

/// The card's own copy of the name, as opposed to the card itself or the
/// inspector's copy beside it.
fn card_title(ui: &AppWindow) -> ElementHandle {
    let card = widest(ui, LONG_NAME).expect("the instance has a card");
    let right = card.absolute_position().x + card.size().width;
    ElementHandle::find_by_accessible_label(ui, LONG_NAME)
        .filter(|element| element.accessible_role() != Some(AccessibleRole::Button))
        .find(|element| {
            element.absolute_position().x >= card.absolute_position().x
                && element.absolute_position().x < right
        })
        .expect("the card shows the name it is labeled with")
}

fn instances(ui: &AppWindow, rows: &[InstanceRow]) {
    ui.set_loading(false);
    ui.set_page(0);
    ui.set_workspace(SharedString::default());
    ui.set_show_inspector(true);
    ui.set_instances(ModelRc::new(VecModel::from(rows.to_vec())));
    ui.set_selected_instance_index(0);
    ui.set_selected_instance_id(rows[0].id.clone());
    settle(ui);
}

fn connectable() -> InstanceRow {
    InstanceRow {
        snapshot_deletion_note: "Deleting files also deletes 2 retained snapshots.".into(),
        id: "repro-client".into(),
        name: LONG_NAME.into(),
        minecraft: "1.16.5".into(),
        loader: "Forge 36.2.0".into(),
        loader_kind: "forge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        source: "Imported from a modpack".into(),
        path: "/tmp/ephervex/instances/repro-client".into(),
        updated: "Just now".into(),
        linked_servers: 1,
        mods: 84,
        needs_attention: false,
        state: "Ready".into(),
        state_kind: "ready".into(),
        can_launch: true,
        can_kill: false,
        connect_server_id: "repro-server".into(),
        connect_server_name: LONG_SERVER.into(),
        can_connect: true,
        creating_server_detail: SharedString::default(),
        creating_server_progress: 0.0,
        creating_server_indeterminate: false,
        restart_stage: SharedString::default(),
        can_restart: false,
    }
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
