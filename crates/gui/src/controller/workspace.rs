//! The host workspace as the window shows it: the layout's panes, and one view
//! of each host they show.
//!
//! A host view is built once per host, however many panes show it, from the
//! host's own row. Nothing here owns host or feature state; it only decides
//! which host and surface each pane is on.

use std::rc::Rc;

use ephervex_core::InstalledHost;
use ephervex_i18n::lookup;
use slint::{Model as _, ModelRc, VecModel};

use super::HostLifecycle;
use super::log_views::LogViews;
use crate::backend;
use crate::ui::{
    AppWindow, HostView, InstanceRow, ServerRow, WorkspacePaneRow, WorkspaceSurfaceRow,
};
use crate::workspace_layout::{PaneId, Surface, WorkspaceLayout};

/// The workspace's layout, and the rows its host views are built from.
pub(super) struct WorkspaceState {
    layout: Option<WorkspaceLayout>,
    /// Every client's and server's row from the newest snapshot, before the
    /// lists' own filters: a pane shows its host whatever those lists hide.
    clients: Vec<InstanceRow>,
    servers: Vec<ServerRow>,
    host_views: Rc<VecModel<HostView>>,
    panes: Rc<VecModel<WorkspacePaneRow>>,
    /// Each host kind's navigation, built once so a re-rendered host view
    /// keeps the same model and its tabs are not rebuilt.
    client_surfaces: ModelRc<WorkspaceSurfaceRow>,
    server_surfaces: ModelRc<WorkspaceSurfaceRow>,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            layout: None,
            clients: Vec::new(),
            servers: Vec::new(),
            host_views: Rc::new(VecModel::default()),
            panes: Rc::new(VecModel::default()),
            client_surfaces: surface_rows(Surface::client()),
            server_surfaces: surface_rows(Surface::server()),
        }
    }
}

/// What a pane showed before a change and shows after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PaneChange {
    pub before: Option<(InstalledHost, Surface)>,
    pub after: Option<(InstalledHost, Surface)>,
}

impl WorkspaceState {
    pub(super) fn attach(&self, ui: &AppWindow) {
        ui.set_host_views(ModelRc::from(Rc::clone(&self.host_views)));
        ui.set_workspace_panes(ModelRc::from(Rc::clone(&self.panes)));
    }

    pub(super) const fn layout(&self) -> Option<&WorkspaceLayout> {
        self.layout.as_ref()
    }

    /// Takes the newest rows of every host.
    pub(super) fn set_rows(&mut self, clients: Vec<InstanceRow>, servers: Vec<ServerRow>) {
        self.clients = clients;
        self.servers = servers;
    }

    /// Replaces the workspace with one pane on `host`.
    pub(super) fn open(&mut self, host: InstalledHost, surface: Surface) -> PaneChange {
        let before = self.single_content();
        let layout = WorkspaceLayout::single(host, surface);
        let after = layout
            .panes()
            .first()
            .map(|placed| (placed.pane.host(), placed.pane.surface()));
        self.layout = Some(layout);
        PaneChange { before, after }
    }

    pub(super) fn close(&mut self) -> Option<WorkspaceLayout> {
        self.layout.take()
    }

    /// Applies `change` to the layout and reports what `pane` showed before and after.
    pub(super) fn change_pane(
        &mut self,
        pane: PaneId,
        change: impl FnOnce(&mut WorkspaceLayout) -> bool,
    ) -> Option<PaneChange> {
        let layout = self.layout.as_mut()?;
        let before = layout.pane(pane).map(|pane| (pane.host(), pane.surface()));
        if !change(layout) {
            return None;
        }
        let after = layout.pane(pane).map(|pane| (pane.host(), pane.surface()));
        Some(PaneChange { before, after })
    }

    /// Whether some pane shows `surface` of `host`.
    pub(super) fn shows(&self, host: InstalledHost, surface: Surface) -> bool {
        self.layout.as_ref().is_some_and(|layout| {
            layout
                .panes()
                .iter()
                .any(|placed| placed.pane.host() == host && placed.pane.surface() == surface)
        })
    }

    /// What the only pane shows, when there is exactly one.
    pub(super) fn single_content(&self) -> Option<(InstalledHost, Surface)> {
        let layout = self.layout.as_ref()?;
        let panes = layout.panes();
        match panes.as_slice() {
            [only] => Some((only.pane.host(), only.pane.surface())),
            _ => None,
        }
    }

    /// Writes the panes and one view per shown host into the window.
    pub(super) fn render(&self, ui: &AppWindow, lifecycle: &HostLifecycle, logs: &LogViews) {
        let Some(layout) = &self.layout else {
            ui.set_workspace_open(false);
            sync_rows(&self.panes, Vec::new());
            sync_rows(&self.host_views, Vec::new());
            return;
        };
        let hosts = layout.hosts();
        let views = hosts
            .iter()
            .map(|&host| self.host_view(host, lifecycle))
            .collect();
        let panes = layout
            .panes()
            .iter()
            .map(|placed| {
                let id = placed.pane.id();
                WorkspacePaneRow {
                    id: i32::try_from(id.get()).unwrap_or(i32::MAX),
                    host: hosts
                        .iter()
                        .position(|&host| host == placed.pane.host())
                        .and_then(|index| i32::try_from(index).ok())
                        .unwrap_or(-1),
                    log: logs.slot_of(id),
                    surface: placed.pane.surface().id().into(),
                    column: i32::from(placed.placement.column),
                    row: i32::from(placed.placement.row),
                    stacked: placed.placement.stacked,
                    can_split_beside: layout.can_split_beside(id),
                    can_split_below: layout.can_split_below(id),
                    can_close: layout.can_close(id),
                }
            })
            .collect();
        sync_rows(&self.host_views, views);
        sync_rows(&self.panes, panes);
        let back = match hosts.first() {
            Some(InstalledHost::Server(_)) if layout.way_back().is_some() => "gui-back-instance",
            Some(InstalledHost::Server(_)) => "gui-back-servers",
            _ => "gui-back-instances",
        };
        ui.set_workspace_back_label(lookup(back).into());
        ui.set_workspace_open(true);
    }

    fn host_view(&self, host: InstalledHost, lifecycle: &HostLifecycle) -> HostView {
        let id = host.id();
        let mut view = HostView {
            kind: host.kind().into(),
            id: id.as_str().into(),
            surfaces: match host {
                InstalledHost::Instance(_) => self.client_surfaces.clone(),
                InstalledHost::Server(_) => self.server_surfaces.clone(),
            },
            ..HostView::default()
        };
        match host {
            InstalledHost::Instance(_) => {
                if let Some(row) = self.clients.iter().find(|row| row.id == id.as_str()) {
                    view.client = row.clone();
                    lifecycle.stamp_client(&mut view.client);
                    view.available = true;
                }
            }
            InstalledHost::Server(_) => {
                if let Some(row) = self.servers.iter().find(|row| row.id == id.as_str()) {
                    view.server = row.clone();
                    lifecycle.stamp_server(&mut view.server);
                    view.available = true;
                }
            }
        }
        view
    }
}

/// The navigation entries of one host kind, as the Surface model lists them.
fn surface_rows(surfaces: &[Surface]) -> ModelRc<WorkspaceSurfaceRow> {
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

/// Writes `rows` into `model` where they differ, so an unchanged pane keeps its
/// component and whatever it was in the middle of.
fn sync_rows<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    while model.row_count() > rows.len() {
        model.remove(model.row_count() - 1);
    }
    for (index, row) in rows.into_iter().enumerate() {
        if index < model.row_count() {
            if model.row_data(index).as_ref() != Some(&row) {
                model.set_row_data(index, row);
            }
        } else {
            model.push(row);
        }
    }
}

/// The host a pane callback names.
pub(super) fn parse_host(kind: &str, id: &str) -> Option<InstalledHost> {
    backend::parse_mod_host(kind, id)
}

/// A pane id as the markup passes it.
pub(super) fn pane_id(value: i32) -> Option<PaneId> {
    u32::try_from(value).ok().map(PaneId::from_raw)
}
