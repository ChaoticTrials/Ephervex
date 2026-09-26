//! The split host workspace as the window draws it: where each shape puts its
//! panes, the controls that change the shape, and which host each pane's
//! header acts on.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{
    AppWindow, GuiI18n, InstanceRow, ServerRow, Theme, WorkspaceHostChoice, WorkspacePaneRow,
};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{
    ComponentHandle, LogicalPosition, LogicalSize, ModelRc, PhysicalSize, SharedString, VecModel,
};

const CLIENT: i32 = 0;
const SERVER: i32 = 1;

/// A pane at one position of a split, labeled by its id.
fn placed(
    id: i32,
    host: i32,
    surface: &str,
    column: i32,
    row: i32,
    stacked: bool,
) -> WorkspacePaneRow {
    WorkspacePaneRow {
        id,
        host,
        log: -1,
        surface: surface.into(),
        label: label(id).into(),
        host_choice: host,
        surface_choice: 0,
        column,
        row,
        stacked,
        can_split_beside: false,
        can_split_below: !stacked,
        can_close: true,
    }
}

fn label(pane: i32) -> String {
    format!("Pane {pane}")
}

/// Every supported split, each pane on the client's or the server's overview.
fn shapes() -> [(&'static str, Vec<WorkspacePaneRow>); 4] {
    [
        (
            "1:1",
            vec![
                placed(1, CLIENT, "Overview", 0, 0, false),
                placed(2, SERVER, "Overview", 1, 0, false),
            ],
        ),
        (
            "2:1",
            vec![
                placed(1, CLIENT, "Overview", 0, 0, true),
                placed(2, CLIENT, "Overview", 0, 1, true),
                placed(3, SERVER, "Overview", 1, 0, false),
            ],
        ),
        (
            "1:2",
            vec![
                placed(1, CLIENT, "Overview", 0, 0, false),
                placed(2, SERVER, "Overview", 1, 0, true),
                placed(3, SERVER, "Overview", 1, 1, true),
            ],
        ),
        (
            "2:2",
            vec![
                placed(1, CLIENT, "Overview", 0, 0, true),
                placed(2, CLIENT, "Overview", 0, 1, true),
                placed(3, SERVER, "Overview", 1, 0, true),
                placed(4, SERVER, "Overview", 1, 1, true),
            ],
        ),
    ]
}

fn split(ui: &AppWindow, panes: Vec<WorkspacePaneRow>) {
    support::show(
        ui,
        0,
        vec![
            support::client_view(instance(false)),
            support::server_view(server(false)),
        ],
        panes,
    );
    settle(ui);
}

#[derive(Debug, Clone, Copy)]
struct Area {
    position: LogicalPosition,
    size: LogicalSize,
}

impl Area {
    fn of(element: &ElementHandle) -> Self {
        Self {
            position: element.absolute_position(),
            size: element.size(),
        }
    }

    fn right(self) -> f32 {
        self.position.x + self.size.width
    }

    fn bottom(self) -> f32 {
        self.position.y + self.size.height
    }

    fn overlaps(self, other: Self) -> bool {
        self.position.x < other.right()
            && other.position.x < self.right()
            && self.position.y < other.bottom()
            && other.position.y < self.bottom()
    }

    fn contains(self, other: Self) -> bool {
        other.position.x >= self.position.x
            && other.position.y >= self.position.y
            && other.right() <= self.right() + 0.5
            && other.bottom() <= self.bottom() + 0.5
    }
}

fn pane_area(ui: &AppWindow, pane: i32) -> Area {
    let element = ElementHandle::find_by_accessible_label(ui, &label(pane))
        .find(|element| element.accessible_role() == Some(AccessibleRole::Groupbox))
        .unwrap_or_else(|| panic!("no pane labeled {}", label(pane)));
    Area::of(&element)
}

fn find(ui: &AppWindow, name: &str, role: AccessibleRole) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, name)
        .find(|element| element.accessible_role() == Some(role))
}

fn button(ui: &AppWindow, name: &str) -> ElementHandle {
    find(ui, name, AccessibleRole::Button).unwrap_or_else(|| panic!("no button labeled {name}"))
}

fn picker(ui: &AppWindow, message: &str, pane: i32) -> ElementHandle {
    let name = lookup_args(message, [("value", Arg::from(label(pane)))]);
    find(ui, &name, AccessibleRole::Combobox).unwrap_or_else(|| panic!("no picker labeled {name}"))
}

fn pane_action(message: &str, pane: i32) -> String {
    lookup_args(
        "gui-pane-action",
        [
            ("first", Arg::from(lookup(message))),
            ("second", Arg::from(label(pane))),
        ],
    )
}

fn pane_control(message: &str, pane: i32) -> String {
    lookup_args(message, [("value", Arg::from(label(pane)))])
}

/// Every shape draws its panes in two equal columns of one or two equal rows,
/// inside the window, never over one another, and never smaller than a pane
/// can be used at: at the usual window size and at the smallest one allowed.
#[test]
fn every_shape_places_its_panes_apart_and_inside_the_window() {
    let ui = window();
    let theme = ui.global::<Theme>();
    let smallest = LogicalSize::new(
        theme.get_workspace_pane_min_width(),
        theme.get_workspace_pane_min_height(),
    );
    for (width, height) in [(1480_u16, 900_u16), (980, 680)] {
        ui.window()
            .set_size(PhysicalSize::new(u32::from(width), u32::from(height)));
        let window = LogicalSize::new(f32::from(width), f32::from(height));
        for (shape, panes) in shapes() {
            let places: Vec<(i32, i32, i32)> = panes
                .iter()
                .map(|pane| (pane.id, pane.column, pane.row))
                .collect();
            split(&ui, panes);
            let areas: Vec<Area> = places.iter().map(|&(id, ..)| pane_area(&ui, id)).collect();
            let context = format!("{shape} at {width}x{height}");
            assert_apart_and_inside(&context, &areas, window, smallest);
            assert_in_columns(&context, &places, &areas);
        }
    }
}

fn assert_apart_and_inside(
    context: &str,
    areas: &[Area],
    window: LogicalSize,
    smallest: LogicalSize,
) {
    for (index, area) in areas.iter().enumerate() {
        assert!(
            area.size.width >= smallest.width - 0.5 && area.size.height >= smallest.height - 0.5,
            "{context}: pane {index} is {:?}",
            area.size
        );
        assert!(
            area.right() <= window.width && area.bottom() <= window.height,
            "{context}: pane {index} leaves the window"
        );
        for (other_index, other) in areas.iter().enumerate().skip(index + 1) {
            assert!(
                !area.overlaps(*other),
                "{context}: panes {index} and {other_index} overlap"
            );
        }
    }
}

/// Panes of one column share its edge and are stacked in order; the columns
/// are equal and in order; a full-height pane spans the two stacked beside it.
fn assert_in_columns(context: &str, places: &[(i32, i32, i32)], areas: &[Area]) {
    for (&(_, column, row), &area) in places.iter().zip(areas) {
        for (&(_, other_column, other_row), &other) in places.iter().zip(areas) {
            if column == other_column {
                assert!(
                    (area.position.x - other.position.x).abs() < 0.5,
                    "{context}: one column, one edge"
                );
                if row < other_row {
                    assert!(
                        area.bottom() <= other.position.y,
                        "{context}: top above bottom"
                    );
                }
            } else {
                assert!(
                    (area.size.width - other.size.width).abs() < 0.5,
                    "{context}: equal columns"
                );
                if column < other_column {
                    assert!(
                        area.right() <= other.position.x,
                        "{context}: left before right"
                    );
                }
            }
        }
    }
    let alone = |column: i32| places.iter().filter(|place| place.1 == column).count() == 1;
    let whole: Vec<Area> = places
        .iter()
        .zip(areas)
        .filter(|(place, _)| alone(place.1))
        .map(|(_, area)| *area)
        .collect();
    let stacked: Vec<Area> = places
        .iter()
        .zip(areas)
        .filter(|(place, _)| !alone(place.1))
        .map(|(_, area)| *area)
        .collect();
    if let (Some(whole), [top, .., bottom]) = (whole.first(), stacked.as_slice()) {
        assert!(
            (whole.position.y - top.position.y).abs() < 0.5,
            "{context}: tops line up"
        );
        assert!(
            (whole.bottom() - bottom.bottom()).abs() < 0.5,
            "{context}: bottoms line up"
        );
    }
}

/// Each pane's header controls are drawn inside that pane, so a pane's body
/// can never sit under another pane's header.
#[test]
fn each_panes_header_stays_inside_the_pane() {
    let ui = window();
    for (shape, panes) in shapes() {
        let ids: Vec<i32> = panes.iter().map(|pane| pane.id).collect();
        let hosts: Vec<i32> = panes.iter().map(|pane| pane.host).collect();
        split(&ui, panes);
        for (&id, &host) in ids.iter().zip(&hosts) {
            let pane = pane_area(&ui, id);
            let primary = if host == CLIENT {
                "gui-launch"
            } else {
                "gui-start"
            };
            let controls = [
                picker(&ui, "gui-pane-host", id),
                button(&ui, &pane_control("gui-pane-close", id)),
                button(&ui, &pane_action(primary, id)),
            ];
            for control in &controls {
                assert!(
                    pane.contains(Area::of(control)),
                    "{shape}: a control of pane {id} is outside it"
                );
            }
        }
    }
}

/// A single workspace offers one split: a second pane beside it.
#[test]
fn a_single_pane_offers_a_pane_beside_it() {
    let ui = window();
    let splits = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&splits);
    ui.on_split_pane_beside(move |pane| sink.borrow_mut().push(pane));
    support::show_client(&ui, instance(false), "Overview");
    settle(&ui);

    button(&ui, &lookup("gui-workspace-split")).invoke_accessible_default_action();
    assert_eq!(splits.borrow().as_slice(), [1]);
    assert!(
        find(
            &ui,
            &lookup_args("gui-pane-close", [("value", Arg::from(label(1)))]),
            AccessibleRole::Button
        )
        .is_none(),
        "the only pane cannot be closed; leaving it is Back"
    );
}

/// A split offers to stack a pane only under a full-height one, lets every
/// pane close, and never splits beside again.
#[test]
fn split_panes_offer_what_their_shape_allows() {
    let ui = window();
    let below = Rc::new(RefCell::new(Vec::new()));
    let closed = Rc::new(RefCell::new(Vec::new()));
    let below_sink = Rc::clone(&below);
    ui.on_split_pane_below(move |pane| below_sink.borrow_mut().push(pane));
    let closed_sink = Rc::clone(&closed);
    ui.on_close_pane(move |pane| closed_sink.borrow_mut().push(pane));
    let (_, panes) = shapes().into_iter().nth(1).expect("the 2:1 shape");
    split(&ui, panes);

    assert!(find(&ui, &lookup("gui-workspace-split"), AccessibleRole::Button).is_none());
    for stacked in [1, 2] {
        assert!(
            find(
                &ui,
                &pane_control("gui-pane-split-below", stacked),
                AccessibleRole::Button
            )
            .is_none(),
            "a column holds two panes at most"
        );
    }
    button(&ui, &pane_control("gui-pane-split-below", 3)).invoke_accessible_default_action();
    assert_eq!(below.borrow().as_slice(), [3]);
    for pane in [1, 2, 3] {
        button(&ui, &pane_control("gui-pane-close", pane)).invoke_accessible_default_action();
    }
    assert_eq!(closed.borrow().as_slice(), [1, 2, 3]);
}

/// A pane's host picker shows its own host, asks for another, and keeps
/// showing what the pane shows until the answer changes it.
#[test]
fn a_panes_host_picker_shows_and_asks_for_its_host() {
    let ui = window();
    let hosts = Rc::new(RefCell::new(Vec::new()));
    let host_sink = Rc::clone(&hosts);
    ui.on_choose_pane_host(move |pane, kind, id| {
        host_sink
            .borrow_mut()
            .push((pane, kind.to_string(), id.to_string()));
    });
    let (_, panes) = shapes().into_iter().next().expect("the 1:1 shape");
    split(&ui, panes);

    let host_picker = picker(&ui, "gui-pane-host", 2);
    assert_eq!(
        host_picker.accessible_value().as_deref(),
        Some(choice_label("server").as_str())
    );
    assert_eq!(
        picker(&ui, "gui-pane-host", 1)
            .accessible_value()
            .as_deref(),
        Some(choice_label("client").as_str()),
        "each pane names its own host"
    );

    choose(&ui, &host_picker, &choice_label("client"));
    assert_eq!(
        hosts.borrow().as_slice(),
        [(2, "client".to_owned(), "client".to_owned())]
    );
    assert_eq!(
        picker(&ui, "gui-pane-host", 2)
            .accessible_value()
            .as_deref(),
        Some(choice_label("server").as_str()),
        "the pane shows its host until the workspace points it elsewhere"
    );
}

/// The elements labeled `label` with `role` inside one pane.
fn in_pane(ui: &AppWindow, pane: i32, label: &str, role: AccessibleRole) -> Vec<ElementHandle> {
    let area = pane_area(ui, pane);
    ElementHandle::find_by_accessible_label(ui, label)
        .filter(|element| element.accessible_role() == Some(role))
        .filter(|element| area.contains(Area::of(element)))
        .collect()
}

/// A tab switches only its own pane's surface.
#[test]
fn a_panes_tabs_switch_only_that_pane() {
    let ui = window();
    let surfaces = support::follow_surface_requests(&ui);
    let (_, panes) = shapes().into_iter().next().expect("the 1:1 shape");
    split(&ui, panes);

    let console = lookup("gui-server-tab-console");
    in_pane(&ui, 2, &console, AccessibleRole::Button)
        .first()
        .expect("the server pane has a Console tab")
        .invoke_accessible_default_action();
    settle(&ui);
    assert_eq!(surfaces.borrow().last(), Some(&(2, "Console".to_owned())));
    assert_eq!(support::surface(&ui, 1), "Console");
    assert_eq!(
        support::surface(&ui, 0),
        "Overview",
        "the other pane is left alone"
    );
}

/// Tabs that do not fit wait under More, and the selected surface always
/// keeps a tab of its own, however far down the list it is.
#[test]
fn tabs_that_do_not_fit_wait_under_more() {
    let ui = window();
    ui.window().set_size(PhysicalSize::new(980, 680));
    let surfaces = support::follow_surface_requests(&ui);
    let mut panes = vec![
        placed(1, CLIENT, "Overview", 0, 0, false),
        placed(2, SERVER, "Diagnostics", 1, 0, false),
    ];
    panes[1].surface_choice = 7;
    split(&ui, panes);

    let tab = |label: &str| in_pane(&ui, 2, &lookup(label), AccessibleRole::Button).len();
    assert_eq!(
        tab("gui-diagnostics-tab"),
        1,
        "the selected surface keeps its tab"
    );
    assert_eq!(
        tab("gui-server-tab-console"),
        0,
        "a tab that does not fit is not drawn"
    );
    let more = in_pane(
        &ui,
        2,
        &lookup("gui-workspace-more-surfaces"),
        AccessibleRole::Button,
    );
    assert_eq!(more.len(), 1);

    open_more(&ui, &more[0], &lookup("gui-server-tab-console"));
    assert_eq!(surfaces.borrow().last(), Some(&(2, "Console".to_owned())));
    assert_eq!(support::surface(&ui, 1), "Console");

    ui.window().set_size(PhysicalSize::new(1480, 900));
    split(&ui, vec![placed(1, CLIENT, "Overview", 0, 0, false)]);
    assert!(
        find(
            &ui,
            &lookup("gui-workspace-more-surfaces"),
            AccessibleRole::Button
        )
        .is_none(),
        "a single pane at the usual size has room for every tab"
    );
}

/// Opens More and clicks the entry for `value`. The list is a popup placed
/// under More, and an entry's position is reported within that popup.
fn open_more(ui: &AppWindow, more: &ElementHandle, value: &str) {
    more.invoke_accessible_default_action();
    settle(ui);
    let entry = ElementHandle::find_by_accessible_label(ui, value)
        .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .find(|element| element.size().height > 0.0 && element.absolute_position().y < 40.0)
        .unwrap_or_else(|| panic!("no entry {value} under More"));
    let origin = more.absolute_position();
    let within = entry.absolute_position();
    let size = entry.size();
    let position = LogicalPosition::new(
        origin.x + within.x + size.width / 2.0,
        origin.y + more.size().height + 4.0 + within.y + size.height / 2.0,
    );
    let button = PointerEventButton::Left;
    ui.window()
        .dispatch_event(WindowEvent::PointerPressed { position, button });
    ui.window()
        .dispatch_event(WindowEvent::PointerReleased { position, button });
    settle(ui);
}

/// Two panes on one host read the host's one state: a change to it shows in
/// both, and each pane's actions name the pane they belong to.
#[test]
fn two_panes_on_one_host_show_one_state() {
    let ui = window();
    split(
        &ui,
        vec![
            placed(1, CLIENT, "Overview", 0, 0, false),
            placed(2, CLIENT, "Logs", 1, 0, false),
        ],
    );
    for pane in [1, 2] {
        assert!(
            find(
                &ui,
                &pane_action("gui-launch", pane),
                AccessibleRole::Button
            )
            .is_some()
        );
    }
    support::update_host(&ui, 0, |view| view.client = instance(true));
    settle(&ui);
    for pane in [1, 2] {
        assert!(
            find(
                &ui,
                &pane_action("gui-stop-game", pane),
                AccessibleRole::Button
            )
            .is_some(),
            "pane {pane} follows the host"
        );
    }
}

/// A pane's Start, Launch, and Restart act on the host that pane shows.
#[test]
fn header_actions_act_on_their_panes_host() {
    let ui = window();
    let launched = Rc::new(RefCell::new(Vec::new()));
    let launch_sink = Rc::clone(&launched);
    ui.on_launch_client(move |id| launch_sink.borrow_mut().push(id.to_string()));
    let started = Rc::new(RefCell::new(Vec::new()));
    let start_sink = Rc::clone(&started);
    ui.on_server_start(move |id| start_sink.borrow_mut().push(id.to_string()));
    let restarted = Rc::new(RefCell::new(Vec::new()));
    let restart_sink = Rc::clone(&restarted);
    ui.on_server_restart(move |id| restart_sink.borrow_mut().push(id.to_string()));
    let (_, panes) = shapes().into_iter().next().expect("the 1:1 shape");
    split(&ui, panes);

    button(&ui, &pane_action("gui-start", 2)).invoke_accessible_default_action();
    button(&ui, &pane_action("gui-launch", 1)).invoke_accessible_default_action();
    assert_eq!(started.borrow().as_slice(), ["server"]);
    assert_eq!(launched.borrow().as_slice(), ["client"]);

    support::update_host(&ui, 1, |view| view.server = server(true));
    settle(&ui);
    button(&ui, &pane_action("gui-restart", 2)).invoke_accessible_default_action();
    assert_eq!(restarted.borrow().as_slice(), ["server"]);
}

/// A split has one way out, above its panes.
#[test]
fn a_split_leaves_from_its_toolbar() {
    let ui = window();
    let backs = Rc::new(RefCell::new(0));
    let sink = Rc::clone(&backs);
    ui.on_workspace_back(move || *sink.borrow_mut() += 1);
    ui.set_workspace_back_label(lookup("gui-back-instances").into());
    let (_, panes) = shapes().into_iter().next().expect("the 1:1 shape");
    split(&ui, panes);

    let back: Vec<ElementHandle> =
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-back-instances"))
            .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
            .collect();
    assert_eq!(
        back.len(),
        1,
        "one Back for the whole split, not one per pane"
    );
    back[0].invoke_accessible_default_action();
    assert_eq!(*backs.borrow(), 1);
}

fn choice_label(kind: &str) -> String {
    match kind {
        "server" => lookup_args(
            "gui-host-choice-server",
            [("value", Arg::from("Repro Server"))],
        ),
        _ => lookup_args(
            "gui-host-choice-client",
            [("value", Arg::from("Repro Client"))],
        ),
    }
}

/// Picks `value` from a combo box the way a user does: open it, then click
/// the entry. The open list is a popup placed over the box, and an entry's
/// position is reported within that popup.
fn choose(ui: &AppWindow, picker: &ElementHandle, value: &str) {
    picker.invoke_accessible_expand_action();
    settle(ui);
    let entry = ElementHandle::find_by_accessible_label(ui, value)
        .find(|element| element.accessible_role() == Some(AccessibleRole::ListItem))
        .unwrap_or_else(|| panic!("no entry {value} in the open picker"));
    let origin = picker.absolute_position();
    let within = entry.absolute_position();
    let size = entry.size();
    // The fluent popup opens 4px above the box it belongs to.
    let position = LogicalPosition::new(
        origin.x + within.x + size.width / 2.0,
        origin.y - 4.0 + within.y + size.height / 2.0,
    );
    let button = PointerEventButton::Left;
    ui.window()
        .dispatch_event(WindowEvent::PointerPressed { position, button });
    ui.window()
        .dispatch_event(WindowEvent::PointerReleased { position, button });
    settle(ui);
}

fn window() -> AppWindow {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");
    let ui = AppWindow::new().expect("window");
    let global = ui.global::<GuiI18n>();
    global.on_text(|id| lookup(id.as_str()).into());
    global.on_count(|id, count| {
        lookup_args(id.as_str(), [("count", Arg::from(i64::from(count)))]).into()
    });
    global.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.as_str()))]).into()
    });
    global.on_values(|id, first, second| {
        lookup_args(
            id.as_str(),
            [
                ("first", Arg::from(first.as_str())),
                ("second", Arg::from(second.as_str())),
            ],
        )
        .into()
    });
    ui.set_workspace_host_choices(ModelRc::new(VecModel::from(vec![
        WorkspaceHostChoice {
            kind: "client".into(),
            id: "client".into(),
            label: choice_label("client").into(),
        },
        WorkspaceHostChoice {
            kind: "server".into(),
            id: "server".into(),
            label: choice_label("server").into(),
        },
    ])));
    ui.set_workspace_host_labels(ModelRc::new(VecModel::from(vec![
        SharedString::from(choice_label("client")),
        SharedString::from(choice_label("server")),
    ])));
    ui.window().set_size(PhysicalSize::new(1480, 900));
    ui.show().expect("show");
    ui
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn instance(running: bool) -> InstanceRow {
    InstanceRow {
        id: "client".into(),
        name: "Repro Client".into(),
        minecraft: "1.21.1".into(),
        loader: "NeoForge 21.1.249".into(),
        loader_kind: "neoforge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        source: "Created in Ephervex".into(),
        path: "/tmp/ephervex/instances/client".into(),
        state: if running { "Running" } else { "Ready" }.into(),
        state_kind: if running { "running" } else { "ready" }.into(),
        can_launch: !running,
        can_kill: running,
        can_restart: running,
        ..InstanceRow::default()
    }
}

fn server(running: bool) -> ServerRow {
    ServerRow {
        id: "server".into(),
        name: "Repro Server".into(),
        minecraft: "1.21.1".into(),
        loader: "NeoForge 21.1.249".into(),
        loader_kind: "neoforge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        relationship: "Standalone".into(),
        state: if running { "Running" } else { "Stopped" }.into(),
        state_kind: if running { "running" } else { "stopped" }.into(),
        path: "/tmp/ephervex/servers/server".into(),
        installed: true,
        can_start: !running,
        can_stop: running,
        can_restart: running,
        can_kill: running,
        ..ServerRow::default()
    }
}
