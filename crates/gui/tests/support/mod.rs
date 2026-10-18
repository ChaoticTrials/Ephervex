//! Puts hosts into the workspace the way the controller does, for tests that
//! drive the markup on its own.

#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{
    AppWindow, HostView, InstanceRow, ServerRow, WorkspacePaneRow, WorkspaceSurfaceRow,
};
use ephervex_gui::workspace_layout::Surface;
use ephervex_i18n::lookup;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

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
pub(crate) fn show(ui: &AppWindow, page: i32, hosts: Vec<HostView>, panes: Vec<WorkspacePaneRow>) {
    ui.set_loading(false);
    ui.set_page(page);
    ui.set_host_views(ModelRc::new(VecModel::from(hosts)));
    ui.set_workspace_panes(ModelRc::new(VecModel::from(panes)));
    ui.set_workspace_open(true);
}

pub(crate) fn show_client(ui: &AppWindow, row: InstanceRow, surface: &str) {
    show(ui, 0, vec![client_view(row)], vec![pane(1, 0, surface)]);
}

pub(crate) fn show_server(ui: &AppWindow, row: ServerRow, surface: &str) {
    show(ui, 1, vec![server_view(row)], vec![pane(1, 0, surface)]);
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
