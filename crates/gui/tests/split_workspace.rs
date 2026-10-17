//! The split host workspace as the window draws it: where each shape puts its
//! panes, the controls that change the shape, and which host each pane's
//! header acts on.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::presentation;
use ephervex_gui::ui::{
    AppWindow, GuiI18n, HostView, InstanceRow, PropertyRules, ServerRow, Theme,
    WorkspaceHostChoice, WorkspaceLinkedChoice, WorkspacePaneRow,
};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions, mock_elapsed_time,
};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{
    ComponentHandle, LogicalPosition, LogicalSize, Model, ModelRc, PhysicalSize, VecModel,
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

fn host_button(ui: &AppWindow, pane: i32) -> ElementHandle {
    button(ui, &pane_control("gui-pane-host", pane))
}

fn folder_button(ui: &AppWindow, pane: i32, host: i32) -> ElementHandle {
    let folder = if host == CLIENT {
        "gui-open-instance-folder"
    } else {
        "gui-open-server-folder"
    };
    button(ui, &pane_action(folder, pane))
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
                host_button(&ui, id),
                folder_button(&ui, id, host),
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

/// On the smallest window the tabs make room for the folder and the layout
/// controls beside them, so none of them is pushed out of its pane.
#[test]
fn a_panes_tab_row_stays_inside_the_pane_on_the_smallest_window() {
    let ui = window();
    ui.window().set_size(PhysicalSize::new(980, 680));
    for (shape, panes) in shapes() {
        let placed: Vec<(i32, i32, bool)> = panes
            .iter()
            .map(|pane| (pane.id, pane.host, pane.can_split_below))
            .collect();
        split(&ui, panes);
        for (id, host, below) in placed {
            let pane = pane_area(&ui, id);
            let mut controls = vec![
                folder_button(&ui, id, host),
                button(&ui, &pane_control("gui-pane-close", id)),
            ];
            if below {
                controls.push(button(&ui, &pane_control("gui-pane-split-below", id)));
            }
            controls.extend(in_pane(
                &ui,
                id,
                &lookup("gui-workspace-more-surfaces"),
                AccessibleRole::Button,
            ));
            for control in &controls {
                assert!(
                    pane.contains(Area::of(control)),
                    "{shape}: {:?} of pane {id} is outside it",
                    control.accessible_label()
                );
            }
        }
    }
}

/// A split pane's header opens the folder of the host that pane shows, named
/// for that host's kind, so two panes never open each other's folder.
#[test]
fn a_panes_folder_button_opens_the_folder_of_its_own_host() {
    let ui = window();
    let opened = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&opened);
    ui.on_open_client_folder(move |id| sink.borrow_mut().push(("client", id.to_string())));
    let sink = Rc::clone(&opened);
    ui.on_open_server_folder(move |id| sink.borrow_mut().push(("server", id.to_string())));
    let (_, panes) = shapes().into_iter().next().expect("the 1:1 shape");
    split(&ui, panes);

    button(&ui, &pane_action("gui-open-server-folder", 2)).invoke_accessible_default_action();
    button(&ui, &pane_action("gui-open-instance-folder", 1)).invoke_accessible_default_action();
    assert_eq!(
        opened.borrow().as_slice(),
        [
            ("server", "server".to_owned()),
            ("client", "client".to_owned())
        ]
    );
    for (message, pane) in [
        ("gui-open-instance-folder", 2),
        ("gui-open-server-folder", 1),
    ] {
        assert!(
            find(&ui, &pane_action(message, pane), AccessibleRole::Button).is_none(),
            "pane {pane} names the folder of the host it shows"
        );
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

/// Hovering a pane's glyph buttons names what they do and nothing more. The
/// pane they belong to is already in front of the eye; only assistive
/// technology needs it spelled out.
#[test]
fn a_panes_glyph_buttons_name_only_their_action_on_hover() {
    let ui = window();
    let (_, panes) = shapes().into_iter().next().expect("the 1:1 shape");
    split(&ui, panes);
    for (label, tooltip) in [
        (
            pane_action("gui-open-server-folder", 2),
            lookup("gui-open-server-folder"),
        ),
        (
            pane_control("gui-pane-split-below", 2),
            lookup("gui-pane-split-below-tooltip"),
        ),
        (
            pane_control("gui-pane-close", 2),
            lookup("gui-pane-close-tooltip"),
        ),
    ] {
        let shown = |text: &str| {
            ElementHandle::find_by_accessible_label(&ui, text)
                .filter(|element| element.size().width > 0.0)
                .count()
        };
        let before = shown(&tooltip);
        button(&ui, &label).mock_single_click(PointerEventButton::Left);
        mock_elapsed_time(std::time::Duration::from_millis(600));
        settle(&ui);
        assert_eq!(shown(&tooltip), before + 1, "hovering {label}");
        assert_eq!(shown(&label), 1, "only the button itself carries {label}");
    }
}

/// A pane's host button names its host and opens the host picker for that
/// pane; a card, or a linked server on its client's card, points the pane at
/// that host.
#[test]
fn a_panes_host_button_opens_a_picker_for_that_pane() {
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
    let names = |pane: i32, name: &str| in_pane(&ui, pane, name, AccessibleRole::Text).len();
    assert!(names(2, "Repro Server") > 0 && names(2, "Repro Client") == 0);
    assert!(names(1, "Repro Client") > 0 && names(1, "Repro Server") == 0);

    host_button(&ui, 2).invoke_accessible_default_action();
    settle(&ui);
    assert_eq!(ui.get_modal(), "pane-host");
    assert_eq!(ui.get_pane_host_picker_pane(), 2);
    assert_eq!(
        (ui.get_pane_host_picker_kind(), ui.get_pane_host_picker_id()),
        ("server".into(), "server".into()),
        "the picker knows what the pane shows now"
    );
    button(&ui, &choice_label("client")).invoke_accessible_default_action();
    settle(&ui);
    assert_eq!(ui.get_modal(), "");

    host_button(&ui, 2).invoke_accessible_default_action();
    settle(&ui);
    button(
        &ui,
        &lookup_args(
            "gui-host-choice-server",
            [("value", Arg::from("Linked Server"))],
        ),
    )
    .invoke_accessible_default_action();
    assert_eq!(
        hosts.borrow().as_slice(),
        [
            (2, "client".to_owned(), "client".to_owned()),
            (2, "server".to_owned(), "linked".to_owned())
        ]
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
    panes[1].surface_choice = 8;
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

/// A client host whose stored runtime settings differ from the other's, with
/// the same revision every host has after its first load.
fn runtime_host(id: &str, min_memory: &str) -> HostView {
    let mut view = support::client_view(InstanceRow {
        id: id.into(),
        ..instance(false)
    });
    view.runtime_memory_min = min_memory.into();
    view.runtime_revision = 1;
    view
}

/// A server host whose stored properties differ from the other's, with the
/// same revision every host has after its first load.
fn properties_host(id: &str, motd: &str, port: &str, online_mode: bool) -> HostView {
    let mut view = support::server_view(ServerRow {
        id: id.into(),
        ..server(false)
    });
    view.property_motd = motd.into();
    view.property_max_players = "20".into();
    view.property_difficulty = "normal".into();
    view.property_gamemode = "survival".into();
    view.property_online_mode = online_mode;
    view.property_access = "Localhost".into();
    view.property_port = port.into();
    view.properties_revision = 1;
    view
}

/// Moves the pane at row 0 onto the host at row 0 the way the controller
/// rewrites rows when the pane showing another host is closed: both models
/// keep their first row and drop the last.
fn close_first_pane(ui: &AppWindow, remaining: WorkspacePaneRow, host: HostView) {
    let panes = ui.get_workspace_panes();
    panes.set_row_data(
        0,
        WorkspacePaneRow {
            host: 0,
            ..remaining
        },
    );
    ui.get_host_views().set_row_data(0, host);
    for model in [panes.as_any(), ui.get_host_views().as_any()] {
        if let Some(model) = model.downcast_ref::<VecModel<WorkspacePaneRow>>() {
            model.remove(1);
        } else if let Some(model) = model.downcast_ref::<VecModel<HostView>>() {
            model.remove(1);
        }
    }
    settle(ui);
}

/// What a pane's Settings form saves when Save is pressed: the host's id and
/// the minimum memory.
fn save_runtime(ui: &AppWindow, pane: i32) -> (String, String) {
    let (_, id, min_memory) = save_host_runtime(ui, pane);
    (id, min_memory)
}

/// What a pane's Settings form saves when Save is pressed, with the host's kind.
fn save_host_runtime(ui: &AppWindow, pane: i32) -> (String, String, String) {
    let saved = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&saved);
    ui.on_save_host_runtime(move |kind, id, min_memory, _, _| {
        *sink.borrow_mut() = Some((kind.to_string(), id.to_string(), min_memory.to_string()));
    });
    in_pane(
        ui,
        pane,
        &lookup("gui-instance-runtime-save"),
        AccessibleRole::Button,
    )
    .first()
    .expect("the pane's Save button")
    .invoke_accessible_default_action();
    saved.take().expect("Save reported the form")
}

/// What a pane's Properties form saves when Save is pressed.
fn save_properties(ui: &AppWindow, pane: i32) -> (String, String, String, bool) {
    let saved = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&saved);
    ui.on_save_server_properties(move |id, motd, _, _, _, _, _, _, online, _, _, port, _| {
        *sink.borrow_mut() = Some((id.to_string(), motd.to_string(), port.to_string(), online));
    });
    in_pane(
        ui,
        pane,
        &lookup("gui-save-changes"),
        AccessibleRole::Button,
    )
    .first()
    .expect("the pane's Save button")
    .invoke_accessible_default_action();
    saved.take().expect("Save reported the form")
}

/// Answers the Properties form's choice rules the way the controller does.
fn install_choice_rules(ui: &AppWindow) {
    let rules = ui.global::<PropertyRules>();
    rules.on_choice_model(|key, stored, unset| {
        let entries: Vec<slint::SharedString> =
            presentation::property_choice_model(&key, &stored, unset)
                .into_iter()
                .map(Into::into)
                .collect();
        ModelRc::new(VecModel::from(entries))
    });
    rules.on_choice_index(|key, stored, unset| {
        presentation::property_choice_index(&key, &stored, unset)
    });
    rules.on_choice_value(|key, stored, picked, unset| {
        presentation::property_choice_value(&key, &stored, &picked, unset).into()
    });
    rules.on_is_unset(|unset, key| presentation::property_is_unset(&unset, &key));
    rules.on_without_unset(|unset, key| presentation::property_without_unset(&unset, &key).into());
}

/// A window showing one server's Properties form, whose stored difficulty and
/// game mode are the given text.
fn properties_form(difficulty: &str, gamemode: &str) -> AppWindow {
    let ui = window();
    install_choice_rules(&ui);
    let mut host = properties_host("aaaa", "A", "25565", true);
    host.property_difficulty = difficulty.into();
    host.property_gamemode = gamemode.into();
    support::show(
        &ui,
        0,
        vec![host],
        vec![placed(1, 0, "Properties", 0, 0, false)],
    );
    settle(&ui);
    ui
}

/// The difficulty and game mode combo boxes, in the order the form lays them out.
fn choice_boxes(ui: &AppWindow) -> Vec<ElementHandle> {
    let mut boxes: Vec<ElementHandle> = ElementHandle::find_by_element_type_name(ui, "ChoiceBox")
        .filter(|element| element.accessible_role() == Some(AccessibleRole::Combobox))
        .collect();
    boxes.sort_by(|a, b| {
        let (a, b) = (a.absolute_position(), b.absolute_position());
        (a.y, a.x)
            .partial_cmp(&(b.y, b.x))
            .expect("finite positions")
    });
    // The network row's access box comes first.
    boxes.split_off(boxes.len().saturating_sub(2))
}

fn shown_choices(ui: &AppWindow) -> Vec<String> {
    choice_boxes(ui)
        .iter()
        .map(|element| element.accessible_value().expect("a value").to_string())
        .collect()
}

/// Moves a combo box's selection down by the given number of entries from its
/// open popup, the way the arrow keys do, and closes the popup.
fn pick_choice_down(ui: &AppWindow, which: usize, steps: usize) {
    choice_boxes(ui)[which].invoke_accessible_expand_action();
    settle(ui);
    for key in std::iter::repeat_n(Key::DownArrow, steps).chain([Key::Return]) {
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    }
    settle(ui);
}

/// The difficulty and game mode a pane's Properties form saves when Save is pressed.
fn save_choices(ui: &AppWindow, pane: i32) -> (String, String) {
    let saved = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&saved);
    ui.on_save_server_properties(
        move |_, _, _, difficulty, gamemode, _, _, _, _, _, _, _, _| {
            *sink.borrow_mut() = Some((difficulty.to_string(), gamemode.to_string()));
        },
    );
    in_pane(
        ui,
        pane,
        &lookup("gui-save-changes"),
        AccessibleRole::Button,
    )
    .first()
    .expect("the pane's Save button")
    .invoke_accessible_default_action();
    saved.take().expect("Save reported the form")
}

/// A window showing one server's Properties form for a file that holds none of
/// the gameplay keys.
fn unset_properties_form() -> AppWindow {
    let ui = window();
    install_choice_rules(&ui);
    let mut host = properties_host("aaaa", "", "25565", true);
    host.property_max_players = "".into();
    host.property_difficulty = "".into();
    host.property_gamemode = "".into();
    host.property_unset = "motd,max-players,difficulty,gamemode,allow-flight,pvp,white-list".into();
    support::show(
        &ui,
        0,
        vec![host],
        vec![placed(1, 0, "Properties", 0, 0, false)],
    );
    settle(&ui);
    ui
}

/// The values a pane's Properties form saves when Save is pressed: the
/// difficulty, the game mode and the keys still unset.
fn save_unset(ui: &AppWindow, pane: i32) -> (String, String, String) {
    let saved = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&saved);
    ui.on_save_server_properties(
        move |_, _, _, difficulty, gamemode, _, _, _, _, _, _, _, unset| {
            *sink.borrow_mut() = Some((
                difficulty.to_string(),
                gamemode.to_string(),
                unset.to_string(),
            ));
        },
    );
    in_pane(
        ui,
        pane,
        &lookup("gui-save-changes"),
        AccessibleRole::Button,
    )
    .first()
    .expect("the pane's Save button")
    .invoke_accessible_default_action();
    saved.take().expect("Save reported the form")
}

/// A property the file does not hold is shown as not set and saved as not
/// set, until the user sets it.
#[test]
fn the_properties_form_leaves_a_property_the_file_does_not_hold_unset_until_it_is_set() {
    let ui = unset_properties_form();
    let not_set = lookup("gui-property-unset");
    assert_eq!(shown_choices(&ui), [not_set.as_str(), not_set.as_str()]);
    let (difficulty, gamemode, unset) = save_unset(&ui, 1);
    assert_eq!((difficulty.as_str(), gamemode.as_str()), ("", ""));
    assert_eq!(
        unset,
        "motd,max-players,difficulty,gamemode,allow-flight,pvp,white-list"
    );

    pick_choice_down(&ui, 0, 2);
    assert_eq!(shown_choices(&ui)[0], "easy");
    let (difficulty, _, unset) = save_unset(&ui, 1);
    assert_eq!(difficulty, "easy");
    assert!(
        unset.contains("difficulty"),
        "a picked value leaves the key unset until it is saved"
    );

    in_pane(&ui, 1, &lookup("gui-pvp"), AccessibleRole::Checkbox)
        .first()
        .expect("the pvp switch")
        .invoke_accessible_default_action();
    settle(&ui);
    let (_, _, unset) = save_unset(&ui, 1);
    assert_eq!(
        unset,
        "motd,max-players,difficulty,gamemode,allow-flight,white-list"
    );
}

/// A server that stores difficulty and game mode as numbers has them shown as
/// the names they stand for, saves the numbers unchanged, and keeps them
/// numbers when another name is picked.
#[test]
fn the_properties_form_shows_and_saves_numeric_choice_values_as_numbers() {
    let ui = properties_form("1", "0");
    assert_eq!(shown_choices(&ui), ["easy", "survival"]);
    assert_eq!(save_choices(&ui, 1), ("1".to_owned(), "0".to_owned()));

    pick_choice_down(&ui, 0, 2);
    pick_choice_down(&ui, 1, 1);
    assert_eq!(shown_choices(&ui), ["hard", "creative"]);
    assert_eq!(save_choices(&ui, 1), ("3".to_owned(), "1".to_owned()));
}

/// A stored value no name stands for is shown as it is and saved as it is,
/// until another entry is picked.
#[test]
fn the_properties_form_shows_and_saves_an_unrepresentable_choice_value_as_it_is() {
    let ui = properties_form("hardcore", "7");
    assert_eq!(shown_choices(&ui), ["hardcore", "7"]);
    assert_eq!(
        save_choices(&ui, 1),
        ("hardcore".to_owned(), "7".to_owned())
    );

    pick_choice_down(&ui, 0, 2);
    assert_eq!(shown_choices(&ui)[0], "easy");
    assert_eq!(save_choices(&ui, 1).0, "easy");
}

#[test]
fn the_properties_form_saves_named_choice_values_as_names() {
    let ui = properties_form("normal", "survival");
    assert_eq!(shown_choices(&ui), ["normal", "survival"]);
    pick_choice_down(&ui, 1, 3);
    assert_eq!(
        save_choices(&ui, 1),
        ("normal".to_owned(), "spectator".to_owned())
    );
}

/// A pane's Settings form shows and saves the host the pane shows, also when
/// the pane is pointed at a host whose stored settings carry the same revision.
#[test]
fn a_panes_runtime_form_follows_the_host_the_pane_shows() {
    let ui = window();
    let panes = || {
        vec![
            placed(1, 0, "Settings", 0, 0, false),
            placed(2, 1, "Settings", 1, 0, false),
        ]
    };
    support::show(
        &ui,
        0,
        vec![runtime_host("aaaa", "1024"), runtime_host("bbbb", "2048")],
        panes(),
    );
    settle(&ui);
    assert_eq!(save_runtime(&ui, 1), ("aaaa".to_owned(), "1024".to_owned()));

    let rows = ui.get_workspace_panes();
    rows.set_row_data(
        0,
        WorkspacePaneRow {
            host: 1,
            ..placed(1, 1, "Settings", 0, 0, false)
        },
    );
    settle(&ui);
    assert_eq!(
        save_runtime(&ui, 1),
        ("bbbb".to_owned(), "2048".to_owned()),
        "a pane pointed at another host"
    );
}

/// Closing the pane above leaves the pane below in the first row, which now
/// shows the other host.
#[test]
fn a_runtime_form_follows_the_host_after_the_pane_before_it_closes() {
    let ui = window();
    support::show(
        &ui,
        0,
        vec![runtime_host("aaaa", "1024"), runtime_host("bbbb", "2048")],
        vec![
            placed(1, 0, "Settings", 0, 0, false),
            placed(2, 1, "Settings", 1, 0, false),
        ],
    );
    settle(&ui);
    close_first_pane(
        &ui,
        placed(2, 0, "Settings", 0, 0, false),
        runtime_host("bbbb", "2048"),
    );
    assert_eq!(save_runtime(&ui, 2), ("bbbb".to_owned(), "2048".to_owned()));
}

/// A server has a Settings tab of its own: its runtime form and its pack-mod
/// setting change the server, beside a client's that change the client.
#[test]
fn a_servers_settings_save_its_own_runtime_beside_a_clients() {
    let ui = window();
    // Tall enough that the pack-mod setting, below the Java runtime section,
    // lies inside each pane.
    ui.window().set_size(PhysicalSize::new(2400, 2400));
    let mut server_view = support::server_view(ServerRow {
        id: "cccc".into(),
        ..server(false)
    });
    server_view.runtime_memory_min = "3072".into();
    server_view.runtime_revision = 1;
    support::show(
        &ui,
        0,
        vec![runtime_host("aaaa", "1024"), server_view],
        vec![
            placed(1, 0, "Settings", 0, 0, false),
            placed(2, 1, "Settings", 1, 0, false),
        ],
    );
    settle(&ui);

    assert_eq!(
        save_host_runtime(&ui, 1),
        ("client".to_owned(), "aaaa".to_owned(), "1024".to_owned())
    );
    assert_eq!(
        save_host_runtime(&ui, 2),
        ("server".to_owned(), "cccc".to_owned(), "3072".to_owned())
    );
    // Each pane offers its own host's pack-mod setting, and switching it
    // names that host.
    let switched = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&switched);
    ui.on_set_allow_pack_mod_version_changes(move |kind, id, allow| {
        sink.borrow_mut()
            .push((kind.to_string(), id.to_string(), allow));
    });
    for (pane, label) in [
        (1, "gui-allow-pack-mod-changes-toggle"),
        (2, "gui-allow-pack-mod-changes-server-toggle"),
    ] {
        let area = pane_area(&ui, pane);
        ElementHandle::find_by_accessible_label(&ui, &lookup(label))
            .find(|element| {
                element.size().width > 0.0
                    && element.accessible_role() == Some(AccessibleRole::Checkbox)
                    && area.contains(Area::of(element))
            })
            .unwrap_or_else(|| panic!("pane {pane} offers {label}"))
            .invoke_accessible_default_action();
    }
    assert_eq!(
        switched.borrow().as_slice(),
        [
            ("client".to_owned(), "aaaa".to_owned(), true),
            ("server".to_owned(), "cccc".to_owned(), true),
        ]
    );
}

/// A pane's Properties form shows and saves the host the pane shows, not the
/// network exposure or message of the server it showed before.
#[test]
fn a_panes_properties_form_follows_the_host_the_pane_shows() {
    let ui = window();
    // Large enough that the form's Save button lies inside its pane.
    ui.window().set_size(PhysicalSize::new(2400, 2400));
    support::show(
        &ui,
        0,
        vec![
            properties_host("aaaa", "Server A", "25565", true),
            properties_host("bbbb", "Server B", "25570", false),
        ],
        vec![
            placed(1, 0, "Properties", 0, 0, false),
            placed(2, 1, "Properties", 1, 0, false),
        ],
    );
    settle(&ui);
    assert_eq!(
        save_properties(&ui, 1),
        (
            "aaaa".to_owned(),
            "Server A".to_owned(),
            "25565".to_owned(),
            true
        )
    );

    close_first_pane(
        &ui,
        placed(2, 0, "Properties", 0, 0, false),
        properties_host("bbbb", "Server B", "25570", false),
    );
    assert_eq!(
        save_properties(&ui, 2),
        (
            "bbbb".to_owned(),
            "Server B".to_owned(),
            "25570".to_owned(),
            false
        )
    );
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

/// A pane's host button and its actions stay inside the pane and clear of one
/// another at every window and text size, whether the host is stopped or
/// running. A pane wide enough keeps them on one row.
#[test]
fn a_panes_host_and_actions_stay_inside_the_pane_and_apart() {
    let ui = window();
    for (width, height) in [(1480_u32, 900_u32), (980, 680)] {
        ui.window().set_size(PhysicalSize::new(width, height));
        // The default text size, then the largest one offered.
        for (scale, default_text) in [(1.0, true), (1.25, false)] {
            ui.global::<Theme>().set_text_scale(scale);
            for running in [false, true] {
                for (shape, panes) in shapes() {
                    let ids: Vec<(i32, i32)> =
                        panes.iter().map(|pane| (pane.id, pane.host)).collect();
                    support::show(
                        &ui,
                        0,
                        vec![
                            support::client_view(instance(running)),
                            support::server_view(server(running)),
                        ],
                        panes,
                    );
                    settle(&ui);
                    let context = format!("{shape} at {width}x{height}, text {scale}");
                    let one_row = width == 1480 && default_text;
                    for (id, host) in ids {
                        assert_host_row_fits(&ui, &context, (id, host), running, one_row);
                    }
                }
            }
        }
    }
}

fn assert_host_row_fits(
    ui: &AppWindow,
    context: &str,
    (id, host): (i32, i32),
    running: bool,
    one_row: bool,
) {
    let pane = pane_area(ui, id);
    let host_button = host_button(ui, id);
    let primary = match (host == CLIENT, running) {
        (true, false) => "gui-launch",
        (true, true) => "gui-stop-game",
        (false, false) => "gui-start",
        (false, true) => "gui-stop",
    };
    let mut actions = vec![
        button(ui, &pane_action("gui-restart", id)),
        button(ui, &pane_action(primary, id)),
    ];
    if host == SERVER {
        actions.push(button(ui, &pane_action("gui-kill", id)));
    }
    for control in actions.iter().chain([&host_button]) {
        assert!(
            pane.contains(Area::of(control)),
            "{context}: {:?} of pane {id} is outside it",
            control.accessible_label()
        );
    }
    for (index, action) in actions.iter().enumerate() {
        for other in actions.iter().skip(index + 1).chain([&host_button]) {
            assert!(
                !Area::of(action).overlaps(Area::of(other)),
                "{context}: {:?} and {:?} of pane {id} overlap",
                action.accessible_label(),
                other.accessible_label()
            );
        }
    }
    if one_row {
        let row = Area::of(&host_button).position.y;
        assert!(
            actions
                .iter()
                .all(|action| (Area::of(action).position.y - row).abs() < 0.5),
            "{context}: the actions of pane {id} left the host's row"
        );
    }
}

/// A workflow's banner is left to the pane that shows the workflow's own
/// surface, and every pane on the host carries it when none does.
#[test]
fn a_workflow_banner_is_left_to_the_pane_showing_the_workflow() {
    let ui = window();
    split(
        &ui,
        vec![
            placed(1, CLIENT, "Isolation", 0, 0, false),
            placed(2, CLIENT, "Mods", 1, 0, false),
        ],
    );
    let banner =
        |pane: i32| in_pane(&ui, pane, "Isolation in progress", AccessibleRole::Text).len();
    support::update_host(&ui, 0, |view| {
        view.workflow_id = "mod-isolation".into();
        view.workflow_title = "Isolation in progress".into();
        view.workflow_pane = 1;
    });
    settle(&ui);
    assert_eq!((banner(1), banner(2)), (1, 0));

    support::update_host(&ui, 0, |view| view.workflow_pane = 0);
    settle(&ui);
    assert_eq!((banner(1), banner(2)), (1, 1));
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
    let choice = |kind: &str, id: &str, name: &str| WorkspaceHostChoice {
        kind: kind.into(),
        id: id.into(),
        name: name.into(),
        ..WorkspaceHostChoice::default()
    };
    let mut client = choice("client", "client", "Repro Client");
    client.linked = ModelRc::new(VecModel::from(vec![WorkspaceLinkedChoice {
        id: "linked".into(),
        name: "Linked Server".into(),
        ..WorkspaceLinkedChoice::default()
    }]));
    ui.set_workspace_client_choices(ModelRc::new(VecModel::from(vec![client])));
    ui.set_workspace_server_choices(ModelRc::new(VecModel::from(vec![choice(
        "server",
        "server",
        "Repro Server",
    )])));
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
