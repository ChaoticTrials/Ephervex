//! Puts hosts into the workspace the way the controller does, for tests that
//! drive the markup on its own.

#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{
    AppWindow, GuiI18n, HostView, InstanceRow, ServerRow, Theme, WorkspacePaneRow,
    WorkspaceSurfaceRow,
};
use ephervex_gui::workspace_layout::Surface;
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, Model, ModelRc, PhysicalSize, VecModel};

fn surfaces(surfaces: &[Surface]) -> ModelRc<WorkspaceSurfaceRow> {
    ModelRc::new(VecModel::from(
        surfaces
            .iter()
            .map(|surface| WorkspaceSurfaceRow {
                id: surface.id().into(),
                label: lookup(surface.label()).into(),
            })
            .collect::<Vec<_>>(),
    ))
}

pub(crate) fn client_view(row: InstanceRow) -> HostView {
    HostView {
        kind: "client".into(),
        id: row.id.clone(),
        available: true,
        client: row,
        surfaces: surfaces(Surface::client()),
        ..HostView::default()
    }
}

pub(crate) fn server_view(row: ServerRow) -> HostView {
    HostView {
        kind: "server".into(),
        id: row.id.clone(),
        available: true,
        server: row,
        surfaces: surfaces(Surface::server()),
        ..HostView::default()
    }
}

/// One pane showing the host view at `host`, alone in the workspace.
pub(crate) fn pane(id: i32, host: i32, surface: &str) -> WorkspacePaneRow {
    WorkspacePaneRow {
        id,
        host,
        surface: surface.into(),
        can_split_beside: true,
        ..WorkspacePaneRow::default()
    }
}

/// Opens the workspace on these host views and panes.
pub(crate) fn show(ui: &AppWindow, hosts: Vec<HostView>, panes: Vec<WorkspacePaneRow>) {
    ui.set_loading(false);
    ui.set_settings_open(false);
    ui.set_host_views(ModelRc::new(VecModel::from(hosts)));
    ui.set_workspace_panes(ModelRc::new(VecModel::from(panes)));
    ui.set_workspace_open(true);
}

pub(crate) fn show_client(ui: &AppWindow, row: InstanceRow, surface: &str) {
    show(ui, vec![client_view(row)], vec![pane(1, 0, surface)]);
}

pub(crate) fn show_server(ui: &AppWindow, row: ServerRow, surface: &str) {
    show(ui, vec![server_view(row)], vec![pane(1, 0, surface)]);
}

/// Opens one settings page over whatever the middle column showed.
pub(crate) fn open_settings(ui: &AppWindow, tab: &str) {
    ui.set_loading(false);
    ui.invoke_open_settings(tab.into());
}

/// Shows Home: no settings and no host.
pub(crate) fn open_home(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_settings_open(false);
    ui.set_workspace_open(false);
}

/// The window size tests lay the shell out at, which is not below its minimum.
pub(crate) const WINDOW_SIZE: (u32, u32) = (1480, 900);

/// A shown window on the software renderer with the application's own text, at
/// a size no smaller than the one the shell is laid out for.
pub(crate) fn test_window() -> AppWindow {
    // The platform is per thread and is set once; a test that opens a second
    // window keeps the first one's.
    let _ = slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })));
    let ui = AppWindow::new().expect("window");
    install_i18n(&ui);
    ui.window()
        .set_size(PhysicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1));
    ui.show().expect("show");
    ui
}

/// The narrowest the application allows the window to be, in pixels at scale 1.
pub(crate) fn minimum_width(ui: &AppWindow) -> u32 {
    // The theme's width is a whole number of logical pixels.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let width = ui.global::<Theme>().get_window_min_width() as u32;
    width
}

/// The smallest the application allows the window to be, in pixels at scale 1.
pub(crate) fn minimum_size(ui: &AppWindow) -> (u32, u32) {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let height = ui.global::<Theme>().get_window_min_height() as u32;
    (minimum_width(ui), height)
}

/// Sizes the window to the narrowest the application allows, `height` tall.
pub(crate) fn set_minimum_width(ui: &AppWindow, height: u32) {
    ui.window()
        .set_size(PhysicalSize::new(minimum_width(ui), height));
}

/// Sizes the window to the smallest the application allows.
pub(crate) fn set_minimum_size(ui: &AppWindow) {
    let (width, height) = minimum_size(ui);
    ui.window().set_size(PhysicalSize::new(width, height));
}

/// The narrowest window that still shows the list column beside the page,
/// which leaves a host page its narrowest, at the smallest height allowed.
pub(crate) fn narrowest_with_list_size(ui: &AppWindow) -> (u32, u32) {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let width = ui.global::<Theme>().get_shell_list_inline_width().ceil() as u32;
    let (_, height) = minimum_size(ui);
    (width, height)
}

/// Sizes the window to `narrowest_with_list_size`.
pub(crate) fn set_narrowest_with_list(ui: &AppWindow) {
    let (width, height) = narrowest_with_list_size(ui);
    ui.window().set_size(PhysicalSize::new(width, height));
}

/// Sizes the window to the narrowest that still shows the side column beside
/// the page, at the smallest height the application allows.
pub(crate) fn set_narrowest_with_side_column(ui: &AppWindow) {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let width = ui.global::<Theme>().get_shell_side_inline_width().ceil() as u32;
    let (_, height) = minimum_size(ui);
    ui.window().set_size(PhysicalSize::new(width, height));
}

/// Answers the window's text lookups from the bundled English messages.
pub(crate) fn install_i18n(ui: &AppWindow) {
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

/// What the pane at `index` shows.
pub(crate) fn surface(ui: &AppWindow, index: usize) -> String {
    ui.get_workspace_panes()
        .row_data(index)
        .map(|pane| pane.surface.to_string())
        .unwrap_or_default()
}

/// Moves a pane to another surface, as the controller does when asked.
pub(crate) fn set_surface(ui: &AppWindow, pane: i32, surface: &str) {
    let panes = ui.get_workspace_panes();
    for index in 0..panes.row_count() {
        if let Some(mut row) = panes.row_data(index)
            && row.id == pane
        {
            let offered =
                if host(ui, usize::try_from(row.host).unwrap_or_default()).kind == "server" {
                    Surface::server()
                } else {
                    Surface::client()
                };
            row.surface_choice = offered
                .iter()
                .position(|offered| offered.id() == surface)
                .and_then(|index| i32::try_from(index).ok())
                .unwrap_or_default();
            row.surface = surface.into();
            panes.set_row_data(index, row);
        }
    }
}

/// Every surface a pane asked for, applied as the controller applies it.
pub(crate) fn follow_surface_requests(ui: &AppWindow) -> Rc<RefCell<Vec<(i32, String)>>> {
    let requests = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&requests);
    let weak = ui.as_weak();
    ui.on_show_pane_surface(move |pane, surface| {
        sink.borrow_mut().push((pane, surface.to_string()));
        if let Some(ui) = weak.upgrade() {
            set_surface(&ui, pane, &surface);
        }
    });
    requests
}

/// Closes the workspace as the controller does: the pane rows go at once, and
/// the pane whose button asked goes with them.
pub(crate) fn follow_workspace_close(ui: &AppWindow) {
    let weak = ui.as_weak();
    ui.on_close_workspace(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_workspace_open(false);
            let panes = ui.get_workspace_panes();
            if let Some(panes) = panes.as_any().downcast_ref::<VecModel<WorkspacePaneRow>>() {
                while panes.row_count() > 0 {
                    panes.remove(panes.row_count() - 1);
                }
            }
        }
    });
}

/// Changes the host view at `index`, as a feature's answer does.
pub(crate) fn update_host(ui: &AppWindow, index: usize, change: impl FnOnce(&mut HostView)) {
    let views = ui.get_host_views();
    if let Some(mut view) = views.row_data(index) {
        change(&mut view);
        views.set_row_data(index, view);
    }
}

/// Puts a mod list on the first host view, as the controller does.
pub(crate) fn set_mods(ui: &AppWindow, rows: ModelRc<ephervex_gui::ui::ModRow>) {
    update_host(ui, 0, |view| view.mods = rows);
}

/// The host view at `index`, as the panes showing it read it.
pub(crate) fn host(ui: &AppWindow, index: usize) -> HostView {
    ui.get_host_views().row_data(index).unwrap_or_default()
}

/// Records an isolation acknowledgement on the first host, as the controller does.
pub(crate) fn follow_acknowledgements(ui: &AppWindow) {
    let weak = ui.as_weak();
    ui.on_acknowledge_isolation(move |_, _, what, accepted| {
        if let Some(ui) = weak.upgrade() {
            update_host(&ui, 0, |view| match what.as_str() {
                "missing" => view.isolation_accept_missing = accepted,
                "linked" => view.isolation_sync_linked_servers = accepted,
                _ => view.isolation_accept_unrecorded = accepted,
            });
        }
    });
}
