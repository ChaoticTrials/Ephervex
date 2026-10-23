//! The host workspace as the window shows it: the layout's panes, and one view
//! of each host they show.
//!
//! A host view is built once per host, however many panes show it: the
//! host's own row, plus what the controller's features last reported about
//! that host. Features update a host's view by host, never by pane, so every
//! pane showing the host reads the same answer.

use std::collections::HashMap;
use std::rc::Rc;

use ephervex_core::{InstalledHost, SearchQuery};
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::{Model as _, ModelRc, SharedString, VecModel};

use super::HostLifecycle;
use super::log_views::LogViews;
use crate::backend;
use crate::ui::{
    AppWindow, HostView, InstanceRow, PendingEntityRow, ServerRow, WorkspaceHostChoice,
    WorkspaceLinkedChoice, WorkspacePaneRow, WorkspaceSurfaceRow,
};
use crate::workspace_layout::{
    LayoutError, PaneId, Placement, Revealed, Shape, Surface, WorkspaceLayout,
};

/// The workspace's layout, and the rows its host views are built from.
pub(super) struct WorkspaceState {
    layout: Option<WorkspaceLayout>,
    /// Every client's and server's row from the newest snapshot, before the
    /// lists' own filters: a pane shows its host whatever those lists hide.
    clients: Vec<InstanceRow>,
    servers: Vec<ServerRow>,
    /// What features reported about each shown host, kept between renders.
    reported: HashMap<InstalledHost, HostView>,
    /// The hosts in the order their views were last written.
    shown: Vec<InstalledHost>,
    host_views: Rc<VecModel<HostView>>,
    panes: Rc<VecModel<WorkspacePaneRow>>,
    /// What a pane's host picker offers: every client, and the servers the
    /// host list shows. A linked server is also offered on its client.
    client_choices: Rc<VecModel<WorkspaceHostChoice>>,
    server_choices: Rc<VecModel<WorkspaceHostChoice>>,
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
            reported: HashMap::new(),
            shown: Vec::new(),
            host_views: Rc::new(VecModel::default()),
            panes: Rc::new(VecModel::default()),
            client_choices: Rc::new(VecModel::default()),
            server_choices: Rc::new(VecModel::default()),
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
        ui.set_workspace_client_choices(ModelRc::from(Rc::clone(&self.client_choices)));
        ui.set_workspace_server_choices(ModelRc::from(Rc::clone(&self.server_choices)));
    }

    pub(super) const fn layout(&self) -> Option<&WorkspaceLayout> {
        self.layout.as_ref()
    }

    /// The servers whose Overview a pane shows, once each, with whether the
    /// server runs.
    pub(super) fn shown_server_overviews(&self) -> Vec<(ephervex_model::ServerId, bool)> {
        let Some(layout) = self.layout.as_ref() else {
            return Vec::new();
        };
        let mut shown = Vec::new();
        for placed in layout.panes() {
            let InstalledHost::Server(server) = placed.pane.host() else {
                continue;
            };
            if placed.pane.surface() != Surface::Overview
                || shown.iter().any(|(known, _)| *known == server)
            {
                continue;
            }
            shown.push((server, self.server_runs(server)));
        }
        shown
    }

    /// Where a host's icon is in the icon pool, or -1.
    pub(super) fn icon_slot(&self, kind: &str, id: &str) -> i32 {
        match kind {
            "client" => self
                .clients
                .iter()
                .find(|row| row.id == id)
                .map(|row| row.icon_slot),
            "server" => self
                .servers
                .iter()
                .find(|row| row.id == id)
                .map(|row| row.icon_slot),
            _ => None,
        }
        .unwrap_or(-1)
    }

    /// Takes the newest rows of every host.
    pub(super) fn set_rows(&mut self, clients: Vec<InstanceRow>, servers: Vec<ServerRow>) {
        self.clients = clients;
        self.servers = servers;
        // What a server's console reported belongs to the run that printed it.
        for row in self
            .servers
            .iter()
            .filter(|row| row.state_kind != "running")
        {
            if let Some(server) = backend::parse_server_id(&row.id)
                && let Some(reported) = self.reported.get_mut(&InstalledHost::Server(server))
            {
                crate::server_statistics_view::forget_console_values(&mut reported.statistics);
            }
        }
    }

    /// Whether a server's row says it runs.
    pub(super) fn server_runs(&self, server: ephervex_model::ServerId) -> bool {
        let id = server.to_string();
        self.servers
            .iter()
            .any(|row| row.id == id && row.state_kind == "running")
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

    /// Shows `host` without changing any pane, as [`WorkspaceLayout::reveal`]
    /// does. `None` when no workspace is open.
    pub(super) fn reveal(
        &mut self,
        host: InstalledHost,
        surface: Surface,
        from: Option<PaneId>,
    ) -> Option<Result<Revealed, LayoutError>> {
        Some(self.layout.as_mut()?.reveal(host, surface, from))
    }

    /// The client and server of `host`'s pair, when its rows say the pair
    /// starts together.
    pub(super) fn pair_starting_together(
        &self,
        host: InstalledHost,
    ) -> Option<(InstalledHost, InstalledHost)> {
        let id = host.id();
        let (together, client, server) = match host {
            InstalledHost::Instance(_) => {
                let row = self.clients.iter().find(|row| row.id == id.as_str())?;
                (
                    row.pair_start_together,
                    id,
                    row.paired_server_id.to_string(),
                )
            }
            InstalledHost::Server(_) => {
                let row = self.servers.iter().find(|row| row.id == id.as_str())?;
                (
                    row.pair_start_together,
                    row.paired_client_id.to_string(),
                    id,
                )
            }
        };
        if !together {
            return None;
        }
        Some((
            InstalledHost::Instance(backend::parse_instance_id(&client)?),
            InstalledHost::Server(backend::parse_server_id(&server)?),
        ))
    }

    /// The name a host's row carries, or its id when no row names it.
    pub(super) fn host_name(&self, host: InstalledHost) -> String {
        let id = host.id();
        let name = match host {
            InstalledHost::Instance(_) => self
                .clients
                .iter()
                .find(|row| row.id == id.as_str())
                .map(|row| row.name.to_string()),
            InstalledHost::Server(_) => self
                .servers
                .iter()
                .find(|row| row.id == id.as_str())
                .map(|row| row.name.to_string()),
        };
        name.unwrap_or(id)
    }

    /// Changes what a feature reported about `host`, answering whether the
    /// workspace shows that host at all. A host no pane shows keeps nothing:
    /// opening it again asks its features afresh.
    pub(super) fn update(
        &mut self,
        host: InstalledHost,
        change: impl FnOnce(&mut HostView),
    ) -> bool {
        let Some(reported) = self.reported.get_mut(&host) else {
            return false;
        };
        change(reported);
        if let Some(index) = self.shown.iter().position(|&shown| shown == host)
            && let Some(shown_row) = self.host_views.row_data(index)
        {
            let mut row = shown_row.clone();
            change_row(&mut row, reported);
            row.workflow_pane = workflow_pane(self.layout.as_ref(), host, &row.workflow_surface);
            // A rewritten row leaves the lists inside it waiting for the pane to
            // read it again, and Slint drops an in-place change to such a list
            // until then. Only a row that changed is rewritten.
            if row != shown_row {
                self.host_views.set_row_data(index, row);
            }
        }
        true
    }

    /// The hosts the workspace shows, in reading order.
    pub(super) fn shown(&self) -> Vec<InstalledHost> {
        self.shown.clone()
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

    /// The first pane, in reading order, showing `host`.
    pub(super) fn first_pane_showing(&self, host: InstalledHost) -> Option<PaneId> {
        self.layout
            .as_ref()?
            .panes()
            .into_iter()
            .find_map(|placed| (placed.pane.host() == host).then_some(placed.pane.id()))
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

    /// Writes the panes and one view per shown host into the window, and
    /// answers the hosts that entered it, whose features have yet to report.
    pub(super) fn render(
        &mut self,
        ui: &AppWindow,
        lifecycle: &HostLifecycle,
        logs: &LogViews,
    ) -> Vec<InstalledHost> {
        let Some(layout) = &self.layout else {
            ui.set_workspace_open(false);
            self.reported.clear();
            self.shown.clear();
            sync_rows(&self.panes, Vec::new());
            sync_rows(&self.host_views, Vec::new());
            super::stamp_open_hosts(ui);
            return Vec::new();
        };
        let hosts = layout.hosts();
        self.reported.retain(|host, _| hosts.contains(host));
        let entered = hosts
            .iter()
            .copied()
            .filter(|host| !self.reported.contains_key(host))
            .collect::<Vec<_>>();
        for &host in &entered {
            // The lists written in place, made once per host so a write that
            // changes nothing in them does not rebuild them.
            self.reported.insert(
                host,
                HostView {
                    linked_servers: ModelRc::new(VecModel::default()),
                    pending_servers: ModelRc::new(VecModel::default()),
                    mods: ModelRc::new(VecModel::default()),
                    browser_results: ModelRc::new(VecModel::default()),
                    ..HostView::default()
                },
            );
        }
        let pending = ui.get_linked_pending_servers();
        let views: Vec<HostView> = hosts
            .iter()
            .map(|&host| self.host_view(host, lifecycle, &pending))
            .collect();
        let panes = pane_rows(layout, &hosts, &views, logs);
        let (clients, servers) = self.host_choices(
            ui.get_show_linked_servers(),
            ui.get_pane_host_query().as_str(),
        );
        sync_rows(&self.client_choices, clients);
        sync_rows(&self.server_choices, servers);
        sync_rows(&self.host_views, views);
        sync_rows(&self.panes, panes);
        super::stamp_open_hosts(ui);
        let back = back_label(layout);
        self.shown = hosts;
        ui.set_workspace_back_label(
            back.map_or_else(SharedString::default, |key| lookup(key).into()),
        );
        ui.set_workspace_open(true);
        entered
    }

    fn host_view(
        &self,
        host: InstalledHost,
        lifecycle: &HostLifecycle,
        pending: &ModelRc<PendingEntityRow>,
    ) -> HostView {
        let id = host.id();
        let mut view = self.reported.get(&host).cloned().unwrap_or_default();
        view.kind = host.kind().into();
        view.id = id.as_str().into();
        view.surfaces = match host {
            InstalledHost::Instance(_) => self.client_surfaces.clone(),
            InstalledHost::Server(_) => self.server_surfaces.clone(),
        };
        view.workflow_pane = workflow_pane(self.layout.as_ref(), host, &view.workflow_surface);
        view.available = false;
        match host {
            InstalledHost::Instance(_) => {
                if let Some(row) = self.clients.iter().find(|row| row.id == id.as_str()) {
                    view.client = row.clone();
                    lifecycle.stamp_client(&mut view.client);
                    view.available = true;
                }
                let linked = self
                    .servers
                    .iter()
                    .filter(|row| row.linked_client_id == id.as_str())
                    .cloned()
                    .map(|mut row| {
                        lifecycle.stamp_server(&mut row);
                        row
                    })
                    .collect::<Vec<_>>();
                view.linked_servers = reuse(&view.linked_servers, linked);
                view.pending_servers = reuse(&view.pending_servers, pending_for(pending, &id));
            }
            InstalledHost::Server(_) => {
                if let Some(row) = self.servers.iter().find(|row| row.id == id.as_str()) {
                    view.server = row.clone();
                    lifecycle.stamp_server(&mut view.server);
                    view.available = true;
                }
                let paired = self
                    .clients
                    .iter()
                    .find(|row| !row.id.is_empty() && row.id == view.server.paired_client_id);
                (view.pair_state, view.pair_state_kind) = paired
                    .map_or_else(Default::default, |row| {
                        (row.state.clone(), row.state_kind.clone())
                    });
            }
        }
        lifecycle.stamp_pair(&mut view);
        (view.pair_shown, view.pair_room) = pair_placement(self.layout.as_ref(), &view);
        view
    }

    /// What the host picker offers: every client with its linked servers, and
    /// the servers the host list shows, running hosts first. A client
    /// ranks by what runs on its card: itself and a linked server, then
    /// itself, then only a linked server, then nothing. Within a rank,
    /// favorites come first and the lists' own order decides the rest.
    ///
    /// `query` narrows the offer by the same matching the instance list uses.
    /// A client stays when it or one of its linked servers matches, and shows
    /// only the linked servers that match unless the client itself does.
    fn host_choices(
        &self,
        show_linked_servers: bool,
        query: &str,
    ) -> (Vec<WorkspaceHostChoice>, Vec<WorkspaceHostChoice>) {
        let query = SearchQuery::new(query);
        let server_matches = |server: &ServerRow| {
            query.matches_any([
                server.name.as_str(),
                server.minecraft.as_str(),
                server.loader.as_str(),
            ])
        };
        let mut clients: Vec<((u8, bool), WorkspaceHostChoice)> = self
            .clients
            .iter()
            .filter_map(|row| {
                let client_matches = query.matches_any([
                    row.name.as_str(),
                    row.minecraft.as_str(),
                    row.loader.as_str(),
                    row.source.as_str(),
                    row.group_name.as_str(),
                ]);
                let linked_servers = self
                    .servers
                    .iter()
                    .filter(|server| server.linked_client_id == row.id);
                if !client_matches && !linked_servers.clone().any(&server_matches) {
                    return None;
                }
                let linked_running = linked_servers.clone().any(|server| server.can_stop);
                let linked = linked_servers
                    .filter(|server| client_matches || server_matches(server))
                    .map(|server| WorkspaceLinkedChoice {
                        id: server.id.clone(),
                        name: server.name.clone(),
                        state: server.state.clone(),
                        state_kind: server.state_kind.clone(),
                        icon_slot: server.icon_slot,
                    })
                    .collect::<Vec<_>>();
                let choice = WorkspaceHostChoice {
                    kind: "client".into(),
                    id: row.id.clone(),
                    name: row.name.clone(),
                    summary: target_summary(&row.minecraft, &row.loader),
                    loader_kind: row.loader_kind.clone(),
                    state: row.state.clone(),
                    state_kind: row.state_kind.clone(),
                    favorite: row.favorite,
                    icon_slot: row.icon_slot,
                    linked: ModelRc::new(VecModel::from(linked)),
                };
                let rank = match (row.can_kill, linked_running) {
                    (true, true) => 0,
                    (true, false) => 1,
                    (false, true) => 2,
                    (false, false) => 3,
                };
                Some(((rank, !row.favorite), choice))
            })
            .collect();
        let mut servers: Vec<((bool, bool), WorkspaceHostChoice)> = self
            .servers
            .iter()
            .filter(|row| show_linked_servers || row.linked_client_id.is_empty())
            .filter(|row| server_matches(row))
            .map(|row| {
                let choice = WorkspaceHostChoice {
                    kind: "server".into(),
                    id: row.id.clone(),
                    name: row.name.clone(),
                    summary: target_summary(&row.minecraft, &row.loader),
                    loader_kind: row.loader_kind.clone(),
                    state: row.state.clone(),
                    state_kind: row.state_kind.clone(),
                    favorite: row.favorite,
                    icon_slot: row.icon_slot,
                    linked: ModelRc::default(),
                };
                ((!row.can_stop, !row.favorite), choice)
            })
            .collect();
        // Stable, so hosts keep the lists' own order within each group.
        clients.sort_by_key(|(rank, _)| *rank);
        servers.sort_by_key(|(rank, _)| *rank);
        (
            clients.into_iter().map(|(_, choice)| choice).collect(),
            servers.into_iter().map(|(_, choice)| choice).collect(),
        )
    }
}

pub(super) fn target_summary(minecraft: &str, loader: &str) -> SharedString {
    lookup_args(
        "gui-target-summary",
        [
            ("first", Arg::from(minecraft)),
            ("second", Arg::from(loader)),
        ],
    )
    .into()
}

/// One row per pane, in reading order, pointing at the host view it reads.
fn pane_rows(
    layout: &WorkspaceLayout,
    hosts: &[InstalledHost],
    views: &[HostView],
    logs: &LogViews,
) -> Vec<WorkspacePaneRow> {
    let shape = layout.shape();
    layout
        .panes()
        .iter()
        .map(|placed| {
            let id = placed.pane.id();
            let host = placed.pane.host();
            let surface = placed.pane.surface();
            let shown = hosts.iter().position(|&shown| shown == host);
            let name = shown
                .and_then(|index| views.get(index))
                .filter(|view| view.available)
                .map_or_else(
                    || lookup("gui-workspace-host-gone"),
                    |view| host_name(view).to_owned(),
                );
            WorkspacePaneRow {
                id: i32::try_from(id.get()).unwrap_or(i32::MAX),
                host: index(shown),
                log: logs.slot_of(id),
                surface: surface.id().into(),
                label: pane_label(shape, placed.placement, &name, surface).into(),
                surface_choice: index(
                    Surface::offered_by(host)
                        .iter()
                        .position(|&offered| offered == surface),
                )
                .max(0),
                column: i32::from(placed.placement.column),
                row: i32::from(placed.placement.row),
                stacked: placed.placement.stacked,
                can_split_beside: layout.can_split_beside(id),
                can_split_below: layout.can_split_below(id),
                can_close: layout.can_close(id),
            }
        })
        .collect()
}

/// A position as the markup indexes, or -1 for none.
fn index(position: Option<usize>) -> i32 {
    position
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(-1)
}

/// Where Back leaves for: the client a single pane was followed from, or Home
/// for a split. A single pane opened from the list has no Back.
fn back_label(layout: &WorkspaceLayout) -> Option<&'static str> {
    if layout.shape() != Shape::Single {
        Some("gui-back-home")
    } else if layout.way_back().is_some() {
        Some("gui-back-instance")
    } else {
        None
    }
}

/// Whether a pane shows the host `view` is paired with, and whether one could
/// be added for it without changing any pane.
fn pair_placement(layout: Option<&WorkspaceLayout>, view: &HostView) -> (bool, bool) {
    let counterpart = if view.kind == "client" {
        backend::parse_server_id(&view.client.paired_server_id).map(InstalledHost::Server)
    } else {
        backend::parse_instance_id(&view.server.paired_client_id).map(InstalledHost::Instance)
    };
    let (Some(counterpart), Some(layout)) = (counterpart, layout) else {
        return (false, false);
    };
    let shown = layout.hosts().contains(&counterpart);
    let room = shown
        || layout
            .clone()
            .reveal(counterpart, Surface::DEFAULT, None)
            .is_ok();
    (shown, room)
}

/// The first pane, in reading order, showing `host` on the surface its
/// workflow is shown in full on, or 0 when none does. Pane ids start at 1.
fn workflow_pane(layout: Option<&WorkspaceLayout>, host: InstalledHost, surface: &str) -> i32 {
    let Some(surface) = Surface::parse(surface) else {
        return 0;
    };
    layout
        .and_then(|layout| {
            layout
                .panes()
                .into_iter()
                .find(|placed| placed.pane.host() == host && placed.pane.surface() == surface)
        })
        .and_then(|placed| i32::try_from(placed.pane.id().get()).ok())
        .unwrap_or(0)
}

fn host_name(view: &HostView) -> &str {
    if view.kind == "server" {
        view.server.name.as_str()
    } else {
        view.client.name.as_str()
    }
}

/// Where a pane sits, its host, and its surface, as assistive technology reads it.
fn pane_label(shape: Shape, placement: Placement, host: &str, surface: Surface) -> String {
    let surface = lookup(surface.label());
    if shape == Shape::Single {
        return lookup_args(
            "gui-pane-label-single",
            [("host", Arg::from(host)), ("surface", Arg::from(surface))],
        );
    }
    let position = match (placement.column, placement.row, placement.stacked) {
        (0, _, false) => "gui-pane-left",
        (0, 0, true) => "gui-pane-top-left",
        (0, _, true) => "gui-pane-bottom-left",
        (_, _, false) => "gui-pane-right",
        (_, 0, true) => "gui-pane-top-right",
        (_, _, true) => "gui-pane-bottom-right",
    };
    lookup_args(
        "gui-pane-label",
        [
            ("position", Arg::from(lookup(position))),
            ("host", Arg::from(host)),
            ("surface", Arg::from(surface)),
        ],
    )
}

/// Writes what a feature reported into a host view already on screen, keeping
/// the parts only the render owns: the host's identity, row, and navigation.
fn change_row(row: &mut HostView, reported: &HostView) {
    let mut updated = reported.clone();
    updated.kind = row.kind.clone();
    updated.id = row.id.clone();
    updated.available = row.available;
    updated.client = row.client.clone();
    updated.server = row.server.clone();
    updated.surfaces = row.surfaces.clone();
    updated.linked_servers = row.linked_servers.clone();
    updated.pending_servers = row.pending_servers.clone();
    updated.pair_shown = row.pair_shown;
    updated.pair_room = row.pair_room;
    updated.pair_state = row.pair_state.clone();
    updated.pair_state_kind = row.pair_state_kind.clone();
    updated.pair_restart_stage = row.pair_restart_stage.clone();
    *row = updated;
}

/// A client's linked servers that are still being created.
pub(super) fn pending_for(
    pending: &ModelRc<PendingEntityRow>,
    client: &str,
) -> Vec<PendingEntityRow> {
    pending
        .iter()
        .filter(|row| row.linked_client_id == client)
        .collect()
}

/// `rows` in `model` when it can hold them in place, so a list on screen is
/// not rebuilt by a render that changed nothing in it.
pub(super) fn reuse<T: Clone + PartialEq + 'static>(
    model: &ModelRc<T>,
    rows: Vec<T>,
) -> ModelRc<T> {
    if let Some(existing) = model.as_any().downcast_ref::<VecModel<T>>() {
        sync_rows(existing, rows);
        model.clone()
    } else {
        ModelRc::new(VecModel::from(rows))
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn client(id: &str, name: &str, favorite: bool, running: bool) -> InstanceRow {
        InstanceRow {
            id: id.into(),
            name: name.into(),
            minecraft: "1.20.1".into(),
            loader: "Vanilla".into(),
            favorite,
            can_kill: running,
            icon_slot: -1,
            ..InstanceRow::default()
        }
    }

    fn server(id: &str, name: &str, linked_to: &str, favorite: bool) -> ServerRow {
        ServerRow {
            id: id.into(),
            name: name.into(),
            minecraft: "1.20.1".into(),
            loader: "Vanilla".into(),
            linked_client_id: linked_to.into(),
            favorite,
            icon_slot: -1,
            ..ServerRow::default()
        }
    }

    fn state(clients: Vec<InstanceRow>, servers: Vec<ServerRow>) -> WorkspaceState {
        let mut state = WorkspaceState::default();
        state.set_rows(clients, servers);
        state
    }

    fn ids(choices: &[WorkspaceHostChoice]) -> Vec<String> {
        choices.iter().map(|choice| choice.id.to_string()).collect()
    }

    #[test]
    fn favorites_rank_first_within_a_running_rank_and_never_above_a_running_host() {
        let state = state(
            vec![
                client("plain", "Plain", false, false),
                client("fav", "Fav", true, false),
                client("running", "Running", false, true),
            ],
            vec![
                server("s-plain", "Server A", "", false),
                server("s-fav", "Server B", "", true),
            ],
        );
        let (clients, servers) = state.host_choices(false, "");
        assert_eq!(ids(&clients), ["running", "fav", "plain"]);
        assert_eq!(ids(&servers), ["s-fav", "s-plain"]);
        assert!(
            clients
                .iter()
                .find(|choice| choice.id == "fav")
                .unwrap()
                .favorite
        );
    }

    #[test]
    fn the_picker_search_filters_clients_and_their_linked_servers_by_the_list_rules() {
        let state = state(
            vec![
                client("a", "Alpha repro", false, false),
                client("b", "Beta", false, false),
                client("c", "Gamma", false, false),
            ],
            vec![
                server("s1", "Beta Server", "b", false),
                server("s2", "Other", "b", false),
                server("s3", "Standalone", "", false),
            ],
        );

        let (clients, servers) = state.host_choices(false, "ALPHA");
        assert_eq!(ids(&clients), ["a"]);
        assert!(servers.is_empty());

        // A matching linked server keeps its client, and only it is listed.
        let (clients, _) = state.host_choices(false, "beta server");
        assert_eq!(ids(&clients), ["b"]);
        let linked = clients[0]
            .linked
            .iter()
            .map(|server| server.id.to_string())
            .collect::<Vec<_>>();
        assert_eq!(linked, ["s1"]);

        // A matching client keeps all of its linked servers.
        let (clients, _) = state.host_choices(false, "beta");
        assert_eq!(clients[0].linked.row_count(), 2);

        let (clients, servers) = state.host_choices(false, "");
        assert_eq!(clients.len(), 3);
        assert_eq!(ids(&servers), ["s3"]);
    }

    #[test]
    fn the_picker_is_offered_every_host_whatever_the_instance_list_hides() {
        // The workspace is given the rows before the host list's filters,
        // so a filter there cannot hide a host from a pane.
        let state = state(
            vec![
                client("a", "Alpha", false, false),
                client("b", "Beta", false, false),
            ],
            Vec::new(),
        );
        let (clients, _) = state.host_choices(false, "");
        assert_eq!(ids(&clients), ["a", "b"]);
    }
}
