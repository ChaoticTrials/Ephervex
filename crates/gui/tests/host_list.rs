//! The host list's cards and the list around them: one height for any name,
//! the selected style, the problem badge, the state strip, the pending
//! progress, the loader tint, the context menus, and the open request.

#![allow(clippy::expect_used, clippy::panic)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{
    ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, Rgba8Pixel, SharedPixelBuffer,
    SharedString, VecModel,
};

slint::slint! {
    import { ClientCard, ServerCard, PendingCard } from "../ui/host-list/host-card.slint";
    import { HostList } from "../ui/host-list/host-list.slint";
    import { InstanceRow, ServerRow, PendingEntityRow, MenuRow } from "../ui/models.slint";
    import { GuiI18n } from "../ui/globals.slint";
    import { Theme } from "../ui/theme.slint";

    export { InstanceRow, ServerRow, PendingEntityRow, MenuRow, GuiI18n, Theme }

    export component CardProbe inherits Window {
        in property <InstanceRow> client-a;
        in property <InstanceRow> client-b;
        in property <ServerRow> server;
        in property <PendingEntityRow> pending;
        out property <length> height-a: card-a.height;
        out property <length> height-b: card-b.height;
        out property <length> height-server: server-card.height;
        out property <length> height-pending: pending-card.height;
        out property <brush> floor: Theme.background;
        out property <brush> ring: Theme.accent-primary;
        out property <brush> outline: Theme.border-strong;
        out property <brush> danger: Theme.danger;
        out property <brush> running: Theme.state-running;
        out property <brush> stopped: Theme.state-stopped;
        out property <brush> attention: Theme.state-attention;
        out property <brush> building: Theme.state-building;
        out property <brush> server-tone: Theme.state-server;
        out property <brush> chip-forge: Theme.chip-forge;
        out property <brush> chip-neoforge: Theme.chip-neoforge;
        out property <brush> chip-vanilla: Theme.chip-vanilla;
        out property <brush> well: Theme.well;
        out property <brush> fill: Theme.accent-primary;
        out property <string> opened;
        out property <string> quick-viewed;
        callback open-requested(string);
        background: Theme.background;
        VerticalLayout {
            x: 24px;
            y: 24px;
            width: 280px;
            spacing: 24px;
            card-a := ClientCard {
                item: root.client-a;
                open-requested(id) => { root.opened = id; root.open-requested(id); }
                quick-view(id) => { root.quick-viewed = id; }
            }
            card-b := ClientCard {
                item: root.client-b;
                open-requested(id) => { root.opened = id; root.open-requested(id); }
            }
            server-card := ServerCard {
                item: root.server;
                open-requested(id) => { root.opened = id; root.open-requested(id); }
                quick-view(id) => { root.quick-viewed = id; }
            }
            pending-card := PendingCard { item: root.pending; }
        }
    }

    export component ListProbe inherits Window {
        in property <[InstanceRow]> instances;
        in property <[ServerRow]> servers;
        in property <[PendingEntityRow]> pending-instances;
        in property <bool> loading;
        in property <string> startup-error;
        in property <int> instance-total;
        in-out property <string> list-query;
        in property <string> reveal-host-id;
        in property <int> reveal-token;
        out property <string> created;
        in-out property <string> instance-filter: "all";
        in-out property <bool> instance-favorites-only;
        in-out property <int> view-changes;
        in-out property <int> group-requests;
        in property <[MenuRow]> instance-group-entries;
        // What the list asked for, one `name:arguments;` entry per request.
        in-out property <string> log;
        out property <brush> ring: Theme.accent-primary;
        out property <brush> danger: Theme.danger;
        background: Theme.background;
        HostList {
            width: parent.width;
            height: parent.height;
            instances: root.instances;
            servers: root.servers;
            pending-instances: root.pending-instances;
            loading: root.loading;
            startup-error: root.startup-error;
            instance-total: root.instance-total;
            list-query <=> root.list-query;
            reveal-host-id: root.reveal-host-id;
            reveal-token: root.reveal-token;
            instance-filter <=> root.instance-filter;
            instance-favorites-only <=> root.instance-favorites-only;
            instance-view-changed => { root.view-changes += 1; }
            open-groups => { root.group-requests += 1; }
            instance-group-entries: root.instance-group-entries;
            create-instance => { root.created = "instance"; }
            create-server => { root.created = "server"; }
            open-client(id) => { root.log += "open-client:" + id + ";"; }
            toggle-client-favorite(id, on) => { root.log += "toggle-client-favorite:" + id + "," + (on ? "true" : "false") + ";"; }
            set-client-group(id, group) => { root.log += "set-client-group:" + id + "," + group + ";"; }
            new-client-group(id) => { root.log += "new-client-group:" + id + ";"; }
            pick-client-icon(id) => { root.log += "pick-client-icon:" + id + ";"; }
            clear-client-icon(id) => { root.log += "clear-client-icon:" + id + ";"; }
            launch-client(id) => { root.log += "launch-client:" + id + ";"; }
            kill-client(id) => { root.log += "kill-client:" + id + ";"; }
            open-client-folder(id) => { root.log += "open-client-folder:" + id + ";"; }
            create-linked-server(id, name) => { root.log += "create-linked-server:" + id + "," + name + ";"; }
            duplicate-client(id) => { root.log += "duplicate-client:" + id + ";"; }
            make-client-persistent(id) => { root.log += "make-client-persistent:" + id + ";"; }
            rename-client(id, name) => { root.log += "rename-client:" + id + "," + name + ";"; }
            delete-client(row) => { root.log += "delete-client:" + row.id + "," + row.name + ";"; }
            open-server(id) => { root.log += "open-server:" + id + ";"; }
            toggle-server-favorite(id, on) => { root.log += "toggle-server-favorite:" + id + "," + (on ? "true" : "false") + ";"; }
            pick-server-icon(id) => { root.log += "pick-server-icon:" + id + ";"; }
            clear-server-icon(id) => { root.log += "clear-server-icon:" + id + ";"; }
            start-server(id) => { root.log += "start-server:" + id + ";"; }
            stop-server(id) => { root.log += "stop-server:" + id + ";"; }
            restart-server(id) => { root.log += "restart-server:" + id + ";"; }
            kill-server(id, name) => { root.log += "kill-server:" + id + "," + name + ";"; }
            open-server-folder(id) => { root.log += "open-server-folder:" + id + ";"; }
            rename-server(id, name) => { root.log += "rename-server:" + id + "," + name + ";"; }
            delete-server(row) => { root.log += "delete-server:" + row.id + "," + row.name + ";"; }
        }
    }
}

type Pixel = [u8; 3];

macro_rules! install_i18n {
    ($ui:expr) => {{
        let global = $ui.global::<GuiI18n>();
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
    }};
}

fn platform() {
    // Set once per test process; a second call is told it is already set.
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .ok();
}

const CARD_X: i32 = 24;
const CARD_Y: i32 = 24;
const CARD_WIDTH: i32 = 280;

fn card_probe(scale: f32) -> CardProbe {
    platform();
    let ui = CardProbe::new().expect("window");
    install_i18n!(ui);
    ui.global::<Theme>().set_text_scale(scale);
    ui.set_client_a(client("alpha", "Alpha"));
    ui.set_client_b(client("bravo", "Bravo"));
    ui.set_server(server("server", "Survival"));
    ui.set_pending(pending("pending", "Building pack"));
    ui.window().set_size(PhysicalSize::new(340, 760));
    ui.show().expect("show");
    ui
}

fn render<T: ComponentHandle>(ui: &T) -> SharedPixelBuffer<Rgba8Pixel> {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render")
}

fn client(id: &str, name: &str) -> InstanceRow {
    InstanceRow {
        id: id.into(),
        name: name.into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.3.0".into(),
        loader_kind: "forge".into(),
        storage_kind: "temporary".into(),
        state: "Ready".into(),
        state_kind: "ready".into(),
        state_line: "Not yet".into(),
        can_launch: true,
        ..InstanceRow::default()
    }
}

fn server(id: &str, name: &str) -> ServerRow {
    ServerRow {
        id: id.into(),
        name: name.into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.3.0".into(),
        loader_kind: "forge".into(),
        address: "localhost:25565".into(),
        state: "Stopped".into(),
        state_kind: "ready".into(),
        state_line: "Stopped | localhost:25565".into(),
        can_start: true,
        ..ServerRow::default()
    }
}

fn pending(id: &str, name: &str) -> PendingEntityRow {
    PendingEntityRow {
        id: id.into(),
        name: name.into(),
        target: "1.20.1 | Forge".into(),
        detail: "12/40".into(),
        progress: 0.75,
        ..PendingEntityRow::default()
    }
}

fn pixel(image: &SharedPixelBuffer<Rgba8Pixel>, x: i32, y: i32) -> Pixel {
    let width = i32::try_from(image.width()).expect("width");
    let at = image.as_slice()[usize::try_from(y * width + x).expect("inside the image")];
    [at.r, at.g, at.b]
}

fn rgb(brush: &slint::Brush) -> Pixel {
    let color = brush.color();
    [color.red(), color.green(), color.blue()]
}

#[allow(clippy::cast_possible_truncation)]
fn whole(length: f32) -> i32 {
    length.round() as i32
}

fn as_f32(value: i32) -> f32 {
    f32::from(i16::try_from(value).expect("a small coordinate"))
}

/// The top edge of each card in the probe's column.
fn card_tops(ui: &CardProbe) -> [i32; 4] {
    let heights = [
        ui.get_height_a(),
        ui.get_height_b(),
        ui.get_height_server(),
        ui.get_height_pending(),
    ]
    .map(whole);
    let mut tops = [CARD_Y; 4];
    for index in 1..4 {
        tops[index] = tops[index - 1] + heights[index - 1] + 24;
    }
    tops
}

#[test]
fn a_card_keeps_one_height_for_short_and_very_long_text_and_nothing_overflows_it() {
    for scale in [1.0, 1.25] {
        let ui = card_probe(scale);
        let long = "An exceptionally long reproduction name that cannot fit the column ".repeat(4);
        let mut short_row = client("alpha", "Alpha");
        short_row.group_name = "QA".into();
        let mut long_row = client("bravo", &long);
        long_row.group_name = "A group name that is much too long to show".into();
        long_row.loader = "NeoForge 21.1.123456789012345678901234567890".into();
        long_row.minecraft = "1.21.1-pre-release-candidate-long".into();
        long_row.linked_servers = 3;
        long_row.state_line = long.as_str().into();
        long_row.last_run = "3 hours ago".into();
        long_row.needs_attention = true;
        ui.set_client_a(short_row);
        ui.set_client_b(long_row);
        ui.set_server(server("server", &long));
        let image = render(&ui);

        assert_eq!(ui.get_height_a(), ui.get_height_b(), "scale {scale}");
        assert_eq!(ui.get_height_a(), ui.get_height_server(), "scale {scale}");

        // Right of every card and its shadow is the floor, whatever the text.
        let floor = rgb(&ui.get_floor());
        let past = CARD_X + CARD_WIDTH + 6;
        let tops = card_tops(&ui);
        for (index, top) in tops.iter().enumerate().take(3) {
            for y in (top + 4)..(top + whole(ui.get_height_a()) - 4) {
                assert_eq!(
                    pixel(&image, past, y),
                    floor,
                    "scale {scale}: text spilled past card {index} at y {y}"
                );
            }
        }
    }
}

#[test]
fn a_card_for_an_open_host_has_the_selection_ring_and_the_others_do_not() {
    let ui = card_probe(1.0);
    let mut open = client("alpha", "Alpha");
    open.open = true;
    ui.set_client_a(open);
    let image = render(&ui);
    let (ring, floor) = (rgb(&ui.get_ring()), rgb(&ui.get_floor()));
    let tops = card_tops(&ui);
    let middle = tops[0] + whole(ui.get_height_a()) / 2;
    assert_eq!(pixel(&image, CARD_X - 2, middle), ring);
    assert_eq!(pixel(&image, CARD_X + 100, tops[0] - 2), ring);
    assert_eq!(pixel(&image, CARD_X - 2, tops[1] + middle - tops[0]), floor);
}

#[test]
fn the_problem_badge_sits_on_the_top_right_corner_and_only_when_there_are_problems() {
    let ui = card_probe(1.0);
    let mut flagged = client("alpha", "Alpha");
    flagged.problems = 2;
    ui.set_client_a(flagged);
    let image = render(&ui);
    let danger = rgb(&ui.get_danger());
    let tops = card_tops(&ui);
    // Left of the badge's middle, clear of its digit, and above and right of
    // the card's own corner.
    let probe = |top: i32| {
        (CARD_X + CARD_WIDTH - 9..CARD_X + CARD_WIDTH - 6)
            .any(|x| pixel(&image, x, top + 2) == danger)
    };
    assert!(probe(tops[0]), "no badge at the corner");
    assert!(!probe(tops[1]), "a badge without problems");
    // Past the card's right edge and above its top: only the badge is there.
    assert!(
        [(3, -3), (4, -5), (2, -6), (5, -2)]
            .iter()
            .any(|(dx, dy)| pixel(&image, CARD_X + CARD_WIDTH + dx, tops[0] + dy) == danger),
        "the badge does not overhang the corner"
    );
}

#[test]
fn the_strip_follows_the_state_of_the_host() {
    let ui = card_probe(1.0);
    let strip_of = |ui: &CardProbe| {
        let image = render(ui);
        let tops = card_tops(ui);
        (
            pixel(&image, CARD_X + 10, tops[0] + whole(ui.get_height_a()) / 2),
            pixel(
                &image,
                CARD_X + 10,
                tops[2] + whole(ui.get_height_server()) / 2,
            ),
        )
    };
    let mut row = client("alpha", "Alpha");
    row.state_kind = "running".into();
    ui.set_client_a(row.clone());
    assert_eq!(strip_of(&ui).0, rgb(&ui.get_running()));
    row.state_kind = "restarting".into();
    ui.set_client_a(row.clone());
    assert_eq!(strip_of(&ui).0, rgb(&ui.get_building()));
    row.state_kind = "ready".into();
    row.needs_attention = true;
    ui.set_client_a(row.clone());
    assert_eq!(strip_of(&ui).0, rgb(&ui.get_attention()));
    row.needs_attention = false;
    ui.set_client_a(row);
    assert_eq!(strip_of(&ui).0, rgb(&ui.get_stopped()));
    // A server that is not running wears the server color.
    assert_eq!(strip_of(&ui).1, rgb(&ui.get_server_tone()));
}

#[test]
fn a_pending_card_shows_how_far_the_work_is() {
    let ui = card_probe(1.0);
    let image = render(&ui);
    let (fill, well, building) = (
        rgb(&ui.get_fill()),
        rgb(&ui.get_well()),
        rgb(&ui.get_building()),
    );
    let tops = card_tops(&ui);
    assert_eq!(
        pixel(
            &image,
            CARD_X + 10,
            tops[3] + whole(ui.get_height_pending()) / 2
        ),
        building
    );
    // The bar is the one row holding both the fill and the empty well.
    let mut ratio = None;
    for y in tops[3]..(tops[3] + whole(ui.get_height_pending())) {
        let (mut filled, mut empty) = (0, 0);
        for x in CARD_X..(CARD_X + CARD_WIDTH) {
            let at = pixel(&image, x, y);
            filled += i32::from(at == fill);
            empty += i32::from(at == well);
        }
        if filled > 20 && empty > 20 {
            ratio = Some(f64::from(filled) / f64::from(filled + empty));
        }
    }
    let ratio = ratio.expect("the card draws a bar");
    assert!((ratio - 0.75).abs() < 0.03, "the bar is {ratio} full");
}

#[test]
fn a_loader_chip_is_tinted_for_its_loader() {
    let ui = card_probe(1.0);
    let tops = card_tops(&ui);
    let has = |ui: &CardProbe, color: Pixel| {
        let image = render(ui);
        (tops[0]..tops[0] + whole(ui.get_height_a()))
            .any(|y| (CARD_X..CARD_X + CARD_WIDTH).any(|x| pixel(&image, x, y) == color))
    };
    let mut row = client("alpha", "Alpha");
    for (kind, tint) in [
        ("forge", rgb(&ui.get_chip_forge())),
        ("neoforge", rgb(&ui.get_chip_neoforge())),
        ("vanilla", rgb(&ui.get_chip_vanilla())),
    ] {
        row.loader_kind = kind.into();
        ui.set_client_a(row.clone());
        assert!(has(&ui, tint), "{kind} chip is not tinted");
    }
    row.loader_kind = "forge".into();
    ui.set_client_a(row);
    assert!(!has(&ui, rgb(&ui.get_chip_neoforge())));
}

fn find_button(ui: &impl ComponentHandle, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
}

#[test]
fn a_client_menu_offers_every_instance_action() {
    let ui = card_probe(1.0);
    render(&ui);
    find_button(&ui, "Alpha")
        .expect("the card")
        .mock_single_click(PointerEventButton::Right);
    render(&ui);
    for id in [
        "gui-open-workspace",
        "gui-launch",
        "gui-open-folder",
        "gui-create-linked-server",
        "gui-add-favorite",
        "gui-set-group",
        "gui-choose-icon-ellipsis",
        "gui-rename",
        "gui-make-persistent",
        "gui-duplicate-instance",
        "gui-delete-instance",
    ] {
        assert!(
            find_button(&ui, &lookup(id)).is_some(),
            "the client menu has no `{id}` entry"
        );
    }
}

#[test]
fn a_server_menu_offers_every_server_action() {
    let ui = card_probe(1.0);
    render(&ui);
    find_button(&ui, "Survival")
        .expect("the card")
        .mock_single_click(PointerEventButton::Right);
    render(&ui);
    for id in [
        "gui-open-server-workspace",
        "gui-start",
        "gui-restart",
        "gui-open-folder",
        "gui-add-favorite",
        "gui-choose-icon-ellipsis",
        "gui-rename",
        "gui-force-kill",
        "gui-delete-server",
    ] {
        assert!(
            find_button(&ui, &lookup(id)).is_some(),
            "the server menu has no `{id}` entry"
        );
    }
}

#[test]
fn pressing_a_card_asks_to_open_it_and_pressing_its_star_does_not() {
    let ui = card_probe(1.0);
    render(&ui);
    let opened: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&opened);
    ui.on_open_requested(move |id| record.borrow_mut().push(id.to_string()));

    let tops = card_tops(&ui);
    // Over the chip row, which has no control of its own.
    let at = LogicalPosition::new(
        as_f32(CARD_X + 150),
        as_f32(tops[1] + whole(ui.get_height_b()) / 2),
    );
    click(&ui, at);
    assert_eq!(*opened.borrow(), ["bravo"]);

    let star = find_button(
        &ui,
        &lookup_args("gui-favorite-host", [("value", Arg::from("Alpha"))]),
    )
    .expect("the star");
    star.invoke_accessible_default_action();
    assert_eq!(opened.borrow().len(), 1, "the star opened the host");
}

#[test]
fn a_cards_quick_view_button_asks_for_that_host_without_opening_it() {
    let ui = card_probe(1.0);
    render(&ui);
    let opened: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&opened);
    ui.on_open_requested(move |id| record.borrow_mut().push(id.to_string()));

    for (name, id) in [("Alpha", "alpha"), ("Survival", "server")] {
        let button = find_button(
            &ui,
            &lookup_args("gui-quick-view-for", [("value", Arg::from(name))]),
        )
        .expect("the quick view button");
        button.invoke_accessible_default_action();
        assert_eq!(ui.get_quick_viewed(), id);
    }
    assert!(opened.borrow().is_empty(), "the button opened the host");
}

fn click(ui: &CardProbe, position: LogicalPosition) {
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(20));
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    render(ui);
}

fn list_probe(rows: usize) -> ListProbe {
    platform();
    let ui = ListProbe::new().expect("window");
    install_i18n!(ui);
    ui.set_instances(ModelRc::new(VecModel::from(
        (0..rows)
            .map(|index| client(&format!("c{index}"), &format!("Client {index}")))
            .collect::<Vec<_>>(),
    )));
    ui.set_servers(ModelRc::new(VecModel::from(vec![server("s0", "Survival")])));
    ui.set_instance_total(i32::try_from(rows).expect("a small count"));
    ui.window().set_size(PhysicalSize::new(300, 700));
    ui.show().expect("show");
    ui
}

#[test]
fn the_list_has_a_button_that_creates_an_instance_and_one_that_creates_a_server() {
    let ui = list_probe(2);
    render(&ui);
    find_button(&ui, &lookup("gui-modal-new-instance"))
        .expect("the instance button")
        .invoke_accessible_default_action();
    assert_eq!(ui.get_created(), SharedString::from("instance"));
    find_button(&ui, &lookup("gui-modal-new-server"))
        .expect("the server button")
        .invoke_accessible_default_action();
    assert_eq!(ui.get_created(), SharedString::from("server"));
}

#[test]
fn an_empty_list_offers_to_create_and_a_list_the_filters_emptied_offers_to_clear_them() {
    let ui = list_probe(0);
    ui.set_servers(ModelRc::default());
    ui.set_instance_total(0);
    ui.set_loading(false);
    render(&ui);
    assert!(
        find_button(&ui, &lookup("gui-modal-new-instance")).is_some(),
        "the empty list offers to create an instance"
    );
    ui.set_instance_total(3);
    render(&ui);
    assert!(
        find_button(&ui, &lookup("gui-clear-filters")).is_some(),
        "a list the filters emptied offers to clear them"
    );
}

#[test]
fn a_list_that_is_still_loading_shows_no_cards_and_no_empty_state() {
    let ui = list_probe(2);
    ui.set_loading(true);
    render(&ui);
    assert!(find_button(&ui, "Client 0").is_none());
    assert!(find_button(&ui, &lookup("gui-modal-new-instance")).is_none());
}

#[test]
fn the_filter_panel_opens_from_the_filters_button_and_reports_what_it_changes() {
    let ui = list_probe(2);
    render(&ui);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-persistent"))
            .next()
            .is_none()
    );
    find_button(&ui, &lookup("gui-more-filters"))
        .expect("the filters button")
        .invoke_accessible_default_action();
    render(&ui);

    let choice = |label: &str| {
        ElementHandle::find_by_accessible_label(&ui, label)
            .next()
            .expect("the choice")
    };
    choice(&lookup("gui-persistent")).invoke_accessible_default_action();
    assert_eq!(ui.get_instance_filter(), SharedString::from("persistent"));
    assert_eq!(ui.get_view_changes(), 1);

    choice(&lookup("gui-favorites-only")).invoke_accessible_default_action();
    assert!(ui.get_instance_favorites_only());
    assert_eq!(ui.get_view_changes(), 2);

    choice(&lookup("gui-groups")).invoke_accessible_default_action();
    assert_eq!(ui.get_group_requests(), 1);
}

#[test]
fn asking_to_reveal_a_host_scrolls_its_card_into_view() {
    let ui = list_probe(40);
    let mut rows: Vec<InstanceRow> = (0..40)
        .map(|index| client(&format!("c{index}"), &format!("Client {index}")))
        .collect();
    // An open card draws the selection ring, which tells where the card is.
    rows[35].open = true;
    ui.set_instances(ModelRc::new(VecModel::from(rows)));
    let ring = rgb(&ui.get_ring());
    let shows_ring = |ui: &ListProbe| {
        let image = render(ui);
        (0..700).any(|y| (0..300).any(|x| pixel(&image, x, y) == ring))
    };
    assert!(!shows_ring(&ui), "the card starts below the window");
    ui.set_reveal_host_id("c35".into());
    ui.set_reveal_token(1);
    render(&ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(50));
    assert!(shows_ring(&ui), "the card was not scrolled into view");
}

/// A list holding hosts whose menus have every entry enabled somewhere.
fn menu_list() -> ListProbe {
    let ui = list_probe(0);
    let mut alpha = client("alpha", "Alpha");
    alpha.group_id = "g1".into();
    alpha.group_name = "Group one".into();
    alpha.icon_slot = 0;
    let mut bravo = client("bravo", "Bravo");
    bravo.can_launch = false;
    bravo.can_kill = true;
    bravo.favorite = true;
    ui.set_instances(ModelRc::new(VecModel::from(vec![alpha, bravo])));
    let mut survival = server("survival", "Survival");
    survival.can_restart = true;
    survival.can_kill = true;
    survival.icon_slot = 0;
    let mut creative = server("creative", "Creative");
    creative.can_start = false;
    creative.can_stop = true;
    ui.set_servers(ModelRc::new(VecModel::from(vec![survival, creative])));
    ui.set_instance_group_entries(ModelRc::new(VecModel::from(vec![MenuRow {
        id: "g-beta".into(),
        label: "Beta".into(),
        ..MenuRow::default()
    }])));
    ui.window().set_size(PhysicalSize::new(300, 900));
    render(&ui);
    ui
}

/// Opens the menu of the card named `card` and activates its entry `entry`,
/// then returns what the list asked for since the last call.
fn through_menu(ui: &ListProbe, card: &str, entry: &str) -> String {
    ui.set_log("".into());
    find_button(ui, card)
        .expect("the card")
        .mock_single_click(PointerEventButton::Right);
    render(ui);
    find_button(ui, entry)
        .unwrap_or_else(|| panic!("the menu of {card} has no `{entry}` entry"))
        .invoke_accessible_default_action();
    render(ui);
    ui.get_log().to_string()
}

#[test]
fn each_client_menu_entry_asks_for_its_own_action_on_its_own_host() {
    let ui = menu_list();
    let entries = [
        ("gui-open-workspace", "open-client:alpha;"),
        ("gui-launch", "launch-client:alpha;"),
        ("gui-open-folder", "open-client-folder:alpha;"),
        (
            "gui-create-linked-server",
            "create-linked-server:alpha,Alpha;",
        ),
        ("gui-add-favorite", "toggle-client-favorite:alpha,true;"),
        ("gui-choose-icon-ellipsis", "pick-client-icon:alpha;"),
        ("gui-remove-icon", "clear-client-icon:alpha;"),
        ("gui-rename", "rename-client:alpha,Alpha;"),
        ("gui-make-persistent", "make-client-persistent:alpha;"),
        ("gui-duplicate-instance", "duplicate-client:alpha;"),
        ("gui-delete-instance", "delete-client:alpha,Alpha;"),
    ];
    for (entry, expected) in entries {
        assert_eq!(
            through_menu(&ui, "Alpha", &lookup(entry)),
            expected,
            "{entry}"
        );
    }
    assert_eq!(
        through_menu(&ui, "Bravo", &lookup("gui-kill-instance")),
        "kill-client:bravo;"
    );
    assert_eq!(
        through_menu(&ui, "Bravo", &lookup("gui-remove-favorite")),
        "toggle-client-favorite:bravo,false;"
    );
}

#[test]
fn the_group_list_of_a_client_menu_sets_creates_or_clears_a_group_for_that_client() {
    let ui = menu_list();
    // The group list opens from the menu's Set group entry.
    let reach = |entry: &str| {
        ui.set_log("".into());
        find_button(&ui, "Alpha")
            .expect("the card")
            .mock_single_click(PointerEventButton::Right);
        render(&ui);
        find_button(&ui, &lookup("gui-set-group"))
            .expect("the menu leads to groups")
            .invoke_accessible_default_action();
        render(&ui);
        find_button(&ui, entry)
            .unwrap_or_else(|| panic!("the group list has no `{entry}` entry"))
            .invoke_accessible_default_action();
        render(&ui);
        ui.get_log().to_string()
    };
    assert_eq!(reach("Beta"), "set-client-group:alpha,g-beta;");
    assert_eq!(
        reach(&lookup("gui-new-group-ellipsis")),
        "new-client-group:alpha;"
    );
    assert_eq!(
        reach(&lookup("gui-remove-from-group")),
        "set-client-group:alpha,;"
    );
}

#[test]
fn each_server_menu_entry_asks_for_its_own_action_on_its_own_host() {
    let ui = menu_list();
    let entries = [
        (
            "Survival",
            "gui-open-server-workspace",
            "open-server:survival;",
        ),
        ("Survival", "gui-start", "start-server:survival;"),
        ("Survival", "gui-restart", "restart-server:survival;"),
        (
            "Survival",
            "gui-open-folder",
            "open-server-folder:survival;",
        ),
        (
            "Survival",
            "gui-add-favorite",
            "toggle-server-favorite:survival,true;",
        ),
        (
            "Survival",
            "gui-choose-icon-ellipsis",
            "pick-server-icon:survival;",
        ),
        ("Survival", "gui-remove-icon", "clear-server-icon:survival;"),
        ("Survival", "gui-rename", "rename-server:survival,Survival;"),
        (
            "Survival",
            "gui-force-kill",
            "kill-server:survival,Survival;",
        ),
        (
            "Survival",
            "gui-delete-server",
            "delete-server:survival,Survival;",
        ),
        ("Creative", "gui-stop", "stop-server:creative;"),
    ];
    for (card, entry, expected) in entries {
        assert_eq!(through_menu(&ui, card, &lookup(entry)), expected, "{entry}");
    }
}

#[test]
fn pressing_a_card_in_the_list_opens_that_host_and_its_star_toggles_that_favorite() {
    let ui = menu_list();
    for (card, expected) in [
        ("Alpha", "open-client:alpha;"),
        ("Bravo", "open-client:bravo;"),
        ("Survival", "open-server:survival;"),
        ("Creative", "open-server:creative;"),
    ] {
        ui.set_log("".into());
        find_button(&ui, card)
            .expect("the card")
            .mock_single_click(PointerEventButton::Left);
        render(&ui);
        assert_eq!(ui.get_log(), expected, "{card}");
    }
    let star = |name: &str, favorite: bool| {
        let id = if favorite {
            "gui-unfavorite-host"
        } else {
            "gui-favorite-host"
        };
        lookup_args(id, [("value", Arg::from(name))])
    };
    for (label, expected) in [
        (star("Alpha", false), "toggle-client-favorite:alpha,true;"),
        (star("Bravo", true), "toggle-client-favorite:bravo,false;"),
        (
            star("Survival", false),
            "toggle-server-favorite:survival,true;",
        ),
    ] {
        ui.set_log("".into());
        find_button(&ui, &label)
            .unwrap_or_else(|| panic!("no star `{label}`"))
            .invoke_accessible_default_action();
        assert_eq!(ui.get_log(), expected, "{label}");
    }
}

#[test]
fn a_server_card_shows_its_relationship_beside_the_state_line_and_its_address_only_there() {
    let ui = card_probe(1.0);
    let mut linked = server("server", "Survival");
    linked.relationship = "Linked client".into();
    ui.set_server(linked);
    render(&ui);
    let count = |text: &str| ElementHandle::find_by_accessible_label(&ui, text).count();
    assert_eq!(count("Linked client"), 1, "the relationship is shown");
    assert_eq!(
        count("Stopped | localhost:25565"),
        1,
        "the state line has the address"
    );
    assert_eq!(count("localhost:25565"), 0, "no chip repeats the address");
}

#[test]
fn a_card_that_appears_together_with_its_reveal_request_is_still_scrolled_to() {
    let ui = list_probe(2);
    render(&ui);
    let mut rows: Vec<InstanceRow> = (0..30)
        .map(|index| client(&format!("c{index}"), &format!("Client {index}")))
        .collect();
    rows[29].open = true;
    // One turn: the rows that create the card, and the request naming it.
    ui.set_instances(ModelRc::new(VecModel::from(rows)));
    ui.set_reveal_host_id("c29".into());
    ui.set_reveal_token(1);
    render(&ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(50));
    let ring = rgb(&ui.get_ring());
    let image = render(&ui);
    assert!(
        (0..700).any(|y| (0..300).any(|x| pixel(&image, x, y) == ring)),
        "the new card was not scrolled into view"
    );
}

#[test]
fn a_revealed_card_leaves_room_above_it_for_its_problem_badge() {
    let ui = list_probe(0);
    let mut rows: Vec<InstanceRow> = (0..30)
        .map(|index| client(&format!("c{index}"), &format!("Client {index}")))
        .collect();
    rows[10].problems = 3;
    ui.set_instances(ModelRc::new(VecModel::from(rows)));
    ui.set_instance_total(30);
    render(&ui);
    // Scroll to the end, then ask for a card that is above the view.
    ui.set_reveal_host_id("c29".into());
    ui.set_reveal_token(1);
    render(&ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(50));
    render(&ui);
    ui.set_reveal_host_id("c10".into());
    ui.set_reveal_token(2);
    render(&ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(50));
    let image = render(&ui);
    let danger = rgb(&ui.get_danger());
    let rows_with_badge: Vec<i32> = (0..700)
        .filter(|y| (0..300).any(|x| pixel(&image, x, *y) == danger))
        .collect();
    // The badge's fill is about its size minus the outline, all of it in view.
    assert!(
        rows_with_badge.len() >= 14,
        "the badge is cut off: {} rows of it show",
        rows_with_badge.len()
    );
    assert!(rows_with_badge[0] > 0);
}
