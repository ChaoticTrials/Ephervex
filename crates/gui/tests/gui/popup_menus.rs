//! The menus a right-click or a "Launch as" press opens.
//!
//! They are drawn by the application from the active palette, so what is worth
//! proving is the pixels a theme reaches, the keyboard and pointer paths that
//! choose from them, and that a right-click lands wherever a card is pressed
//! rather than only where nothing else is.

use crate::support;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use ephervex_gui::ui::{
    AppWindow, BuiltInPalettes, InstanceRow, MenuRow, PendingEntityRow, ServerRow, Theme,
};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{Color, ComponentHandle, LogicalPosition, ModelRc, Rgba8Pixel, SharedString, VecModel};

#[test]
fn a_card_menu_is_painted_from_the_active_palette() {
    let ui = window();
    let theme = ui.global::<Theme>();
    let mut palette = theme.get_palette();
    palette.overlay = Color::from_rgb_u8(0x12, 0x34, 0x56);
    palette.selection = Color::from_rgb_u8(0x65, 0x43, 0x21);
    theme.set_palette(palette);

    show_instance(&ui, instance());
    let origin = card_point(&ui, "Repro Client");
    right_click(&ui, origin);

    let rename = entry(&ui, &lookup("gui-rename")).expect("the card offers its menu");
    let at = |element: &ElementHandle| {
        let position = element.absolute_position();
        // Inside the row but clear of its label, where only the menu's own
        // fill and the row's highlight can be.
        (
            origin.x + position.x + element.size().width - 6.0,
            origin.y + position.y + element.size().height / 2.0,
        )
    };
    assert_eq!(
        pixel(&ui, at(&rename)),
        (0x12, 0x34, 0x56),
        "the menu takes the palette's overlay color, not the toolkit's own"
    );

    let (x, y) = at(&rename);
    ui.window().dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(x, y),
    });
    settle(&ui);
    assert_eq!(
        pixel(&ui, at(&rename)),
        (0x65, 0x43, 0x21),
        "the row under the pointer takes the palette's selection color"
    );

    let palettes = ui.global::<BuiltInPalettes>();
    theme.set_palette(palettes.get_workbench());
    settle(&ui);
    ui.window().dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(0.0, 0.0),
    });
    settle(&ui);
    let overlay = theme.get_overlay();
    assert_eq!(
        pixel(&ui, at(&rename)),
        (overlay.red(), overlay.green(), overlay.blue()),
        "an open menu follows a change of palette"
    );
}

#[test]
fn the_arrow_keys_skip_disabled_entries_and_enter_picks_the_highlighted_one() {
    let ui = window();
    let folders: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&folders);
    ui.on_open_client_folder(move |id| record.borrow_mut().push(id.to_string()));

    // Idle, so Kill instance is offered but cannot be chosen.
    show_instance(&ui, instance());
    right_click(&ui, card_point(&ui, "Repro Client"));
    assert!(entry(&ui, &lookup("gui-rename")).is_some());

    // Open workspace, Launch, then past the disabled Kill to Open folder.
    for _ in 0..3 {
        press(&ui, Key::DownArrow);
    }
    press(&ui, Key::Return);

    assert_eq!(folders.borrow().as_slice(), ["repro-client"]);
    assert!(
        entry(&ui, &lookup("gui-rename")).is_none(),
        "choosing an entry closes the menu"
    );
}

#[test]
fn the_up_arrow_from_nothing_wraps_to_the_last_entry() {
    let ui = window();
    show_instance(&ui, instance());
    right_click(&ui, card_point(&ui, "Repro Client"));

    press(&ui, Key::UpArrow);
    press(&ui, Key::Return);

    assert_eq!(
        ui.get_modal(),
        "delete-instance",
        "the last entry of an idle instance's menu is its deletion"
    );
}

#[test]
fn escape_closes_a_menu_without_choosing() {
    let ui = window();
    show_instance(&ui, instance());
    right_click(&ui, card_point(&ui, "Repro Client"));
    assert!(entry(&ui, &lookup("gui-rename")).is_some());

    press(&ui, Key::DownArrow);
    press(&ui, Key::Escape);

    assert!(entry(&ui, &lookup("gui-rename")).is_none());
    assert_eq!(ui.get_modal(), "");
}

#[test]
fn a_menu_closes_when_its_card_moves_out_from_under_it() {
    let ui = window();
    let second = InstanceRow {
        id: "second-client".into(),
        name: "Second Client".into(),
        ..instance()
    };
    ui.set_loading(false);
    ui.set_instances(ModelRc::new(VecModel::from(vec![
        instance(),
        second.clone(),
    ])));
    settle(&ui);

    right_click(&ui, card_point(&ui, "Second Client"));
    assert!(entry(&ui, &lookup("gui-rename")).is_some());

    // The first card goes away and the second moves up into its place.
    ui.set_instances(ModelRc::new(VecModel::from(vec![second])));
    settle(&ui);

    assert!(
        entry(&ui, &lookup("gui-rename")).is_none(),
        "a menu left at its old position would sit over unrelated cards"
    );
}

#[test]
fn a_menu_closes_when_its_card_is_pushed_down_by_a_row_above_it() {
    let ui = window();
    show_instance(&ui, instance());

    right_click(&ui, card_point(&ui, "Repro Client"));
    assert!(entry(&ui, &lookup("gui-rename")).is_some());

    // The card survives, and a row appears above it.
    ui.set_pending_instances(ModelRc::new(VecModel::from(vec![PendingEntityRow {
        name: "Building".into(),
        ..PendingEntityRow::default()
    }])));
    settle(&ui);

    assert!(
        entry(&ui, &lookup("gui-rename")).is_none(),
        "a menu left where it was drawn would sit over a different card"
    );
}

#[test]
fn a_disabled_entry_cannot_be_chosen_with_the_pointer() {
    let ui = window();
    let killed: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&killed);
    ui.on_kill_instance(move |id, _| record.borrow_mut().push(id.to_string()));

    show_instance(&ui, instance());
    let origin = card_point(&ui, "Repro Client");
    right_click(&ui, origin);
    let kill = entry(&ui, &lookup("gui-kill-instance")).expect("Kill instance is listed");
    click(&ui, center_in(origin, &kill));

    assert!(killed.borrow().is_empty());
    assert!(
        entry(&ui, &lookup("gui-rename")).is_some(),
        "pressing a disabled entry leaves the menu open"
    );
}

/// A running instance offers a stop where a ready one offers a launch, and the
/// two are not each other's negation.
#[test]
fn a_running_instances_menu_kills_it_and_cannot_launch_it() {
    let ui = window();
    let killed: Rc<RefCell<Vec<String>>> = Rc::default();
    let launched: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&killed);
    ui.on_kill_instance(move |id, _| record.borrow_mut().push(id.to_string()));
    let record = Rc::clone(&launched);
    ui.on_launch_client(move |id| record.borrow_mut().push(id.to_string()));

    let mut running = instance();
    running.can_launch = false;
    running.can_kill = true;
    running.state_kind = "running".into();
    show_instance(&ui, running);
    let origin = card_point(&ui, "Repro Client");
    right_click(&ui, origin);

    let launch = entry(&ui, &lookup("gui-launch")).expect("Launch is listed");
    click(&ui, center_in(origin, &launch));
    assert!(
        launched.borrow().is_empty(),
        "a running instance is not launchable"
    );
    let kill = entry(&ui, &lookup("gui-kill-instance")).expect("Kill instance is listed");
    click(&ui, center_in(origin, &kill));

    assert_eq!(killed.borrow().as_slice(), ["repro-client"]);
}

#[test]
fn a_right_click_on_a_card_star_opens_the_menu_without_toggling_the_favorite() {
    let ui = window();
    let toggled: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&toggled);
    ui.on_set_favorite(move |_, id, _| record.borrow_mut().push(id.to_string()));

    show_instance(&ui, instance());
    let star_label = lookup_args(
        "gui-favorite-host",
        [("value", Arg::from("Repro Client".to_owned()))],
    );
    let star = ElementHandle::find_by_accessible_label(&ui, &star_label)
        .find(|element| element.size().width > 0.0)
        .expect("the card has a star");
    right_click(&ui, center(&star));

    assert!(
        entry(&ui, &lookup("gui-rename")).is_some(),
        "a card offers its menu wherever it is pressed"
    );
    assert!(toggled.borrow().is_empty());
}

#[test]
fn a_right_click_on_a_card_title_opens_the_menu() {
    let ui = window();
    show_instance(&ui, instance());
    // The title has no label of its own to find; the star after it does, and
    // the title ends just before.
    let star_label = lookup_args(
        "gui-favorite-host",
        [("value", Arg::from("Repro Client".to_owned()))],
    );
    let star = ElementHandle::find_by_accessible_label(&ui, &star_label)
        .find(|element| element.size().width > 0.0)
        .expect("the card has a star");
    let position = star.absolute_position();
    right_click(
        &ui,
        LogicalPosition::new(position.x - 40.0, position.y + star.size().height / 2.0),
    );

    assert!(entry(&ui, &lookup("gui-rename")).is_some());
}

#[test]
fn the_group_entry_leads_to_the_groups_to_choose_from() {
    let ui = window();
    let groups: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
    let record = Rc::clone(&groups);
    ui.on_set_client_group(move |id, group| {
        record
            .borrow_mut()
            .push((id.to_string(), group.to_string()));
    });
    ui.set_instance_group_entries(ModelRc::new(VecModel::from(vec![
        menu_row("g-alpha", "Alpha"),
        menu_row("g-beta", "Beta"),
    ])));

    show_instance(&ui, instance());
    let origin = card_point(&ui, "Repro Client");
    right_click(&ui, origin);
    let set_group = entry(&ui, &lookup("gui-set-group")).expect("the menu leads to groups");
    click(&ui, center_in(origin, &set_group));

    assert!(
        entry(&ui, &lookup("gui-rename")).is_none(),
        "the group list replaces the menu it was reached from"
    );
    let remove = entry(&ui, &lookup("gui-remove-from-group")).expect("and ends in its commands");
    assert!(
        entry(&ui, &lookup("gui-new-group-ellipsis")).is_some(),
        "a new group can be made from the same list"
    );
    click(&ui, center_in(origin, &remove));
    assert!(
        groups.borrow().is_empty(),
        "an instance outside every group has none to leave"
    );

    press(&ui, Key::Escape);
    right_click(&ui, origin);
    let set_group = entry(&ui, &lookup("gui-set-group")).expect("the menu opens again");
    click(&ui, center_in(origin, &set_group));
    let beta = entry(&ui, "Beta").expect("Beta is listed");
    click(&ui, center_in(origin, &beta));

    assert_eq!(
        groups.borrow().as_slice(),
        [("repro-client".to_owned(), "g-beta".to_owned())]
    );
}

#[test]
fn a_server_card_offers_its_own_menu() {
    let ui = window();
    show_instance(&ui, instance());
    ui.set_listed_servers(ModelRc::new(VecModel::from(vec![server()])));
    settle(&ui);

    let origin = card_point(&ui, "Repro Server");
    right_click(&ui, origin);
    let rename = entry(&ui, &lookup("gui-rename")).expect("a server card has a menu");
    click(&ui, center_in(origin, &rename));

    assert_eq!(ui.get_modal(), "rename");
}

#[test]
fn launch_as_lists_the_accounts_and_launches_as_the_one_chosen() {
    let ui = window();
    let launched: Rc<RefCell<Vec<(String, String, bool)>>> = Rc::default();
    let record = Rc::clone(&launched);
    ui.on_launch_client_as(move |id, account, alongside| {
        record
            .borrow_mut()
            .push((id.to_string(), account.to_string(), alongside));
    });

    support::show_client(&ui, instance(), "Overview");
    ui.set_account_launch_entries(ModelRc::new(VecModel::from(vec![
        menu_row("alice-id", "Alice"),
        menu_row("bob-id", "Bob"),
    ])));
    settle(&ui);

    let button = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-launch-as"))
        .find(|element| element.size().width > 0.0)
        .expect("the host header offers Launch as");
    click(&ui, center(&button));

    // The list opens under the button, from its left edge.
    let origin = LogicalPosition::new(
        button.absolute_position().x,
        button.absolute_position().y + button.size().height,
    );
    assert!(
        entry(&ui, &lookup("gui-add-account")).is_some(),
        "the accounts are followed by a way to add one"
    );
    let bob = entry(&ui, "Bob").expect("Bob is listed");
    click(&ui, center_in(origin, &bob));

    assert_eq!(
        launched.borrow().as_slice(),
        [("repro-client".to_owned(), "bob-id".to_owned(), false)]
    );
}

fn menu_row(id: &str, label: &str) -> MenuRow {
    MenuRow {
        id: id.into(),
        label: label.into(),
        ..MenuRow::default()
    }
}

fn show_instance(ui: &AppWindow, row: InstanceRow) {
    ui.set_loading(false);
    ui.set_instances(ModelRc::new(VecModel::from(vec![row])));
    settle(ui);
}

/// A point on a card that nothing but the card itself answers: the line under
/// its title, clear of its buttons.
fn card_point(ui: &AppWindow, name: &str) -> LogicalPosition {
    let card = ElementHandle::find_by_accessible_label(ui, name)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .expect("the list holds the card");
    LogicalPosition::new(
        card.absolute_position().x + 160.0,
        card.absolute_position().y + card.size().height - 18.0,
    )
}

/// The row of an open menu carrying `label`.
fn entry(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label).find(|element| {
        element.accessible_role() == Some(AccessibleRole::Button) && element.size().width > 0.0
    })
}

fn center(element: &ElementHandle) -> LogicalPosition {
    LogicalPosition::new(
        element.absolute_position().x + element.size().width / 2.0,
        element.absolute_position().y + element.size().height / 2.0,
    )
}

/// The middle of a row of a menu opened at `origin`. A popup's elements report
/// their positions from the popup's own corner.
fn center_in(origin: LogicalPosition, element: &ElementHandle) -> LogicalPosition {
    LogicalPosition::new(
        origin.x + element.absolute_position().x + element.size().width / 2.0,
        origin.y + element.absolute_position().y + element.size().height / 2.0,
    )
}

fn right_click(ui: &AppWindow, position: LogicalPosition) {
    pointer(ui, position, PointerEventButton::Right);
}

fn click(ui: &AppWindow, position: LogicalPosition) {
    pointer(ui, position, PointerEventButton::Left);
}

fn pointer(ui: &AppWindow, position: LogicalPosition, button: PointerEventButton) {
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    ui.window()
        .dispatch_event(WindowEvent::PointerPressed { position, button });
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    ui.window()
        .dispatch_event(WindowEvent::PointerReleased { position, button });
    settle(ui);
}

fn press(ui: &AppWindow, key: Key) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    settle(ui);
}

/// The color the window shows at a point, as red, green and blue.
fn pixel(ui: &AppWindow, (x, y): (f32, f32)) -> (u8, u8, u8) {
    let buffer = ui.window().take_snapshot().expect("render");
    let width = usize::try_from(buffer.width()).expect("a width");
    // Truncation is the rounding a pixel grid wants here.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let at = y as usize * width + x as usize;
    let Rgba8Pixel { r, g, b, .. } = buffer.as_slice()[at];
    (r, g, b)
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn window() -> AppWindow {
    support::test_window()
}

fn instance() -> InstanceRow {
    InstanceRow {
        id: "repro-client".into(),
        name: "Repro Client".into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.4.10".into(),
        loader_kind: "forge".into(),
        storage: "Persistent".into(),
        storage_kind: "persistent".into(),
        source: "Created in Ephervex".into(),
        path: "/tmp/ephervex/instances/repro-client".into(),
        state: "Ready".into(),
        state_kind: "ready".into(),
        can_launch: true,
        icon_slot: -1,
        ..InstanceRow::default()
    }
}

fn server() -> ServerRow {
    ServerRow {
        id: "repro-server".into(),
        name: "Repro Server".into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.4.10".into(),
        loader_kind: "forge".into(),
        storage: "Persistent".into(),
        storage_kind: "persistent".into(),
        relationship: "Standalone".into(),
        state: "Stopped".into(),
        state_kind: "stopped".into(),
        state_detail: "Not running".into(),
        address: "127.0.0.1:25565".into(),
        installed: true,
        can_start: true,
        icon_slot: -1,
        path: SharedString::from("/tmp/ephervex/servers/repro-server"),
        ..ServerRow::default()
    }
}
