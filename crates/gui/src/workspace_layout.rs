//! Which hosts the desktop workspace shows, where, and on which surface.
//!
//! Composition only. A pane names a host and one of its surfaces; everything
//! the host or the surface's feature knows stays with the controller, keyed by
//! host, so the same host can be shown in several panes without a second copy
//! of its state.

use ephervex_core::InstalledHost;

/// One developer-facing view of a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    Overview,
    Case,
    Console,
    Mods,
    Packs,
    Isolation,
    Settings,
    Properties,
    Snapshots,
    Logs,
    Diagnostics,
}

const CLIENT_SURFACES: [Surface; 9] = [
    Surface::Overview,
    Surface::Case,
    Surface::Mods,
    Surface::Packs,
    Surface::Isolation,
    Surface::Settings,
    Surface::Snapshots,
    Surface::Logs,
    Surface::Diagnostics,
];

const SERVER_SURFACES: [Surface; 10] = [
    Surface::Overview,
    Surface::Console,
    Surface::Mods,
    Surface::Packs,
    Surface::Isolation,
    Surface::Settings,
    Surface::Properties,
    Surface::Snapshots,
    Surface::Logs,
    Surface::Diagnostics,
];

impl Surface {
    /// Where a pane lands when it has nothing better to show. Every host offers it.
    pub const DEFAULT: Self = Self::Overview;

    /// The surfaces a host offers, in the order its navigation lists them.
    #[must_use]
    pub const fn offered_by(host: InstalledHost) -> &'static [Self] {
        match host {
            InstalledHost::Instance(_) => Self::client(),
            InstalledHost::Server(_) => Self::server(),
        }
    }

    /// What every client instance offers.
    #[must_use]
    pub const fn client() -> &'static [Self] {
        &CLIENT_SURFACES
    }

    /// What every local server offers.
    #[must_use]
    pub const fn server() -> &'static [Self] {
        &SERVER_SURFACES
    }

    #[must_use]
    pub fn is_offered_by(self, host: InstalledHost) -> bool {
        Self::offered_by(host).contains(&self)
    }

    /// The stable id the markup compares against.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Case => "Case",
            Self::Console => "Console",
            Self::Mods => "Mods",
            Self::Packs => "Packs",
            Self::Isolation => "Isolation",
            Self::Settings => "Settings",
            Self::Properties => "Properties",
            Self::Snapshots => "Snapshots",
            Self::Logs => "Logs",
            Self::Diagnostics => "Diagnostics",
        }
    }

    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        CLIENT_SURFACES
            .iter()
            .chain(SERVER_SURFACES.iter())
            .copied()
            .find(|surface| surface.id() == id)
    }

    /// The message id of the surface's navigation label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Overview => "gui-instance-tab-overview",
            Self::Case => "gui-instance-tab-case",
            Self::Console => "gui-server-tab-console",
            Self::Mods => "gui-instance-tab-mods",
            Self::Packs => "gui-packs-tab",
            Self::Isolation => "gui-isolation-tab",
            Self::Settings => "gui-instance-tab-settings",
            Self::Properties => "gui-server-tab-properties",
            Self::Snapshots => "gui-snapshots",
            Self::Logs => "gui-instance-tab-logs",
            Self::Diagnostics => "gui-diagnostics-tab",
        }
    }

    /// Whether the surface reads a log stream, which each pane follows on its own.
    #[must_use]
    pub const fn reads_log(self) -> bool {
        matches!(self, Self::Logs | Self::Console)
    }

    /// `self` when the host offers it, otherwise [`Self::DEFAULT`].
    #[must_use]
    pub fn or_default_for(self, host: InstalledHost) -> Self {
        if self.is_offered_by(host) {
            self
        } else {
            Self::DEFAULT
        }
    }
}

/// The first identity of the quick view's pane. The workspace never numbers
/// a pane this high.
const QUICK_VIEW_FIRST_PANE: u32 = 1 << 20;

/// A pane's identity for as long as it exists. Never reused within one layout.
///
/// Composition identity only: no host or feature state is ever keyed by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(u32);

impl PaneId {
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }

    /// Whether this is the quick view's pane rather than one of the workspace's.
    #[must_use]
    pub const fn is_quick_view(self) -> bool {
        self.0 >= QUICK_VIEW_FIRST_PANE
    }
}

/// What one pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pane {
    id: PaneId,
    host: InstalledHost,
    surface: Surface,
}

impl Pane {
    #[must_use]
    pub const fn id(&self) -> PaneId {
        self.id
    }

    #[must_use]
    pub const fn host(&self) -> InstalledHost {
        self.host
    }

    #[must_use]
    pub const fn surface(&self) -> Surface {
        self.surface
    }
}

/// The supported shapes, named columns-first as the left column's pane count
/// and the right column's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Single,
    /// Two full-height panes side by side.
    OneByOne,
    /// Two stacked panes on the left, one full-height pane on the right.
    TwoByOne,
    /// One full-height pane on the left, two stacked panes on the right.
    OneByTwo,
    /// Two stacked panes in each column.
    TwoByTwo,
}

impl Shape {
    #[must_use]
    pub const fn pane_count(self) -> usize {
        match self {
            Self::Single => 1,
            Self::OneByOne => 2,
            Self::TwoByOne | Self::OneByTwo => 3,
            Self::TwoByTwo => 4,
        }
    }
}

/// Where a pane sits: its column, its row within that column, and whether the
/// column holds a second pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub column: u8,
    pub row: u8,
    pub stacked: bool,
}

/// A pane together with where it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacedPane {
    pub pane: Pane,
    pub placement: Placement,
}

/// A layout operation that does not apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    UnknownPane,
    /// The only pane is the workspace itself; leaving it is Back, not close.
    LastPane,
    /// The requested split would leave the supported shapes.
    NoRoom,
    /// The host does not offer that surface.
    SurfaceNotOffered,
}

/// How [`WorkspaceLayout::reveal`] brought a host into view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revealed {
    /// A pane already showed the host, and nothing changed.
    Shown(PaneId),
    /// A new pane shows the host. Every existing pane is where it was, as it was.
    Added(PaneId),
}

/// The one way back a single workspace remembers, set when one host's view
/// opens another host, such as a client opening its linked server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WayBack {
    pub host: InstalledHost,
    pub surface: Surface,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Whole(Pane),
    Stacked { top: Pane, bottom: Pane },
}

impl Column {
    fn panes(&self) -> Vec<Pane> {
        match *self {
            Self::Whole(pane) => vec![pane],
            Self::Stacked { top, bottom } => vec![top, bottom],
        }
    }

    fn contains(&self, id: PaneId) -> bool {
        self.panes().iter().any(|pane| pane.id == id)
    }

    /// The column's pane when it is alone in it.
    const fn whole(&self) -> Option<PaneId> {
        match self {
            Self::Whole(pane) => Some(pane.id),
            Self::Stacked { .. } => None,
        }
    }

    fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        match self {
            Self::Whole(pane) => (pane.id == id).then_some(pane),
            Self::Stacked { top, bottom } => [top, bottom].into_iter().find(|pane| pane.id == id),
        }
    }
}

/// Every value is one of the supported shapes: a single pane, or two columns
/// each holding one or two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrangement {
    Single(Pane),
    Split { left: Column, right: Column },
}

/// The workspace's composition: its shape, its panes, and the way back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLayout {
    arrangement: Arrangement,
    next_id: u32,
    way_back: Option<WayBack>,
}

impl WorkspaceLayout {
    /// A single pane showing `host`, on `surface` when the host offers it.
    #[must_use]
    pub fn single(host: InstalledHost, surface: Surface) -> Self {
        Self::single_from(host, surface, 1)
    }

    /// The one pane of a quick view: like [`Self::single`], but its identity
    /// is in a range no workspace pane uses, so a callback naming a pane never
    /// means the wrong layout.
    #[must_use]
    pub fn quick_view(host: InstalledHost, surface: Surface) -> Self {
        Self::single_from(host, surface, QUICK_VIEW_FIRST_PANE)
    }

    fn single_from(host: InstalledHost, surface: Surface, first: u32) -> Self {
        let pane = Pane {
            id: PaneId(first),
            host,
            surface: surface.or_default_for(host),
        };
        Self {
            arrangement: Arrangement::Single(pane),
            next_id: first + 1,
            way_back: None,
        }
    }

    #[must_use]
    pub const fn shape(&self) -> Shape {
        match self.arrangement {
            Arrangement::Single(_) => Shape::Single,
            Arrangement::Split { left, right } => match (left, right) {
                (Column::Whole(_), Column::Whole(_)) => Shape::OneByOne,
                (Column::Stacked { .. }, Column::Whole(_)) => Shape::TwoByOne,
                (Column::Whole(_), Column::Stacked { .. }) => Shape::OneByTwo,
                (Column::Stacked { .. }, Column::Stacked { .. }) => Shape::TwoByTwo,
            },
        }
    }

    /// Every pane in reading order: the left column top to bottom, then the right.
    #[must_use]
    pub fn panes(&self) -> Vec<PlacedPane> {
        match self.arrangement {
            Arrangement::Single(pane) => vec![PlacedPane {
                pane,
                placement: Placement {
                    column: 0,
                    row: 0,
                    stacked: false,
                },
            }],
            Arrangement::Split { left, right } => [(0, left), (1, right)]
                .into_iter()
                .flat_map(|(column, content)| {
                    let panes = content.panes();
                    let stacked = panes.len() > 1;
                    panes
                        .into_iter()
                        .zip(0..)
                        .map(move |(pane, row)| PlacedPane {
                            pane,
                            placement: Placement {
                                column,
                                row,
                                stacked,
                            },
                        })
                })
                .collect(),
        }
    }

    #[must_use]
    pub fn pane(&self, id: PaneId) -> Option<Pane> {
        self.panes()
            .into_iter()
            .map(|placed| placed.pane)
            .find(|pane| pane.id == id)
    }

    /// Each host shown, once, in the order its first pane is read.
    #[must_use]
    pub fn hosts(&self) -> Vec<InstalledHost> {
        let mut hosts = Vec::new();
        for placed in self.panes() {
            if !hosts.contains(&placed.pane.host) {
                hosts.push(placed.pane.host);
            }
        }
        hosts
    }

    #[must_use]
    pub const fn way_back(&self) -> Option<WayBack> {
        self.way_back
    }

    /// Whether `id` can gain a pane beside it. Only a single pane can: the
    /// workspace has at most two columns.
    #[must_use]
    pub fn can_split_beside(&self, id: PaneId) -> bool {
        matches!(self.arrangement, Arrangement::Single(pane) if pane.id == id)
    }

    /// Whether `id` can gain a pane below it: it has to be alone in one of two
    /// columns, since a column holds at most two panes and a single full-width
    /// column of two is not a supported shape.
    #[must_use]
    pub fn can_split_below(&self, id: PaneId) -> bool {
        match self.arrangement {
            Arrangement::Single(_) => false,
            Arrangement::Split { left, right } => [left, right]
                .into_iter()
                .any(|column| matches!(column, Column::Whole(pane) if pane.id == id)),
        }
    }

    #[must_use]
    pub fn can_close(&self, id: PaneId) -> bool {
        matches!(self.arrangement, Arrangement::Split { .. }) && self.pane(id).is_some()
    }

    /// Adds a pane to the right of the single pane, showing the same host on
    /// its default surface.
    ///
    /// # Errors
    ///
    /// [`LayoutError::NoRoom`] unless `id` is the single pane.
    pub fn split_beside(&mut self, id: PaneId) -> Result<PaneId, LayoutError> {
        let source = self.pane(id).ok_or(LayoutError::UnknownPane)?;
        if !self.can_split_beside(id) {
            return Err(LayoutError::NoRoom);
        }
        let added = self.new_pane(source.host);
        self.arrangement = Arrangement::Split {
            left: Column::Whole(source),
            right: Column::Whole(added),
        };
        self.way_back = None;
        Ok(added.id)
    }

    /// Adds a pane under `id`, showing the same host on its default surface.
    ///
    /// # Errors
    ///
    /// [`LayoutError::NoRoom`] unless `id` is alone in one of two columns.
    pub fn split_below(&mut self, id: PaneId) -> Result<PaneId, LayoutError> {
        let source = self.pane(id).ok_or(LayoutError::UnknownPane)?;
        if !self.can_split_below(id) {
            return Err(LayoutError::NoRoom);
        }
        let added = self.new_pane(source.host);
        if let Arrangement::Split { left, right } = &mut self.arrangement {
            for column in [left, right] {
                if matches!(column, Column::Whole(pane) if pane.id == id) {
                    *column = Column::Stacked {
                        top: source,
                        bottom: added,
                    };
                }
            }
        }
        Ok(added.id)
    }

    /// Closes `id` and settles on the supported shape the rest form.
    ///
    /// The survivors keep their reading order: a stacked column that loses a
    /// pane keeps the other whole, and a column that disappears beside a
    /// stacked one leaves that stack as two side-by-side panes.
    ///
    /// # Errors
    ///
    /// [`LayoutError::LastPane`] for the single pane.
    pub fn close(&mut self, id: PaneId) -> Result<(), LayoutError> {
        if self.pane(id).is_none() {
            return Err(LayoutError::UnknownPane);
        }
        let Arrangement::Split { left, right } = self.arrangement else {
            return Err(LayoutError::LastPane);
        };
        self.arrangement = match (without(left, id), without(right, id)) {
            (Some(left), Some(right)) => Arrangement::Split { left, right },
            (Some(remaining), None) | (None, Some(remaining)) => match remaining {
                Column::Whole(pane) => Arrangement::Single(pane),
                Column::Stacked { top, bottom } => Arrangement::Split {
                    left: Column::Whole(top),
                    right: Column::Whole(bottom),
                },
            },
            (None, None) => return Err(LayoutError::UnknownPane),
        };
        self.way_back = None;
        Ok(())
    }

    /// Points `id` at another host. The surface stays when that host offers
    /// it and falls back to the default when it does not.
    ///
    /// # Errors
    ///
    /// [`LayoutError::UnknownPane`] for a pane this layout does not have.
    pub fn show_host(&mut self, id: PaneId, host: InstalledHost) -> Result<(), LayoutError> {
        let pane = self.pane_mut(id).ok_or(LayoutError::UnknownPane)?;
        pane.surface = pane.surface.or_default_for(host);
        pane.host = host;
        self.way_back = None;
        Ok(())
    }

    /// Switches `id` to another surface of the host it already shows.
    ///
    /// # Errors
    ///
    /// [`LayoutError::SurfaceNotOffered`] when the pane's host has no such surface.
    pub fn show_surface(&mut self, id: PaneId, surface: Surface) -> Result<(), LayoutError> {
        let pane = self.pane_mut(id).ok_or(LayoutError::UnknownPane)?;
        if !surface.is_offered_by(pane.host) {
            return Err(LayoutError::SurfaceNotOffered);
        }
        pane.surface = surface;
        Ok(())
    }

    /// Follows a link from what `id` shows to another host's surface.
    ///
    /// In a single workspace this remembers where it came from, so Back
    /// returns there. In a split the pane is simply pointed elsewhere.
    ///
    /// # Errors
    ///
    /// [`LayoutError::UnknownPane`] for a pane this layout does not have.
    pub fn follow(
        &mut self,
        id: PaneId,
        host: InstalledHost,
        surface: Surface,
    ) -> Result<(), LayoutError> {
        let single = matches!(self.arrangement, Arrangement::Single(_));
        let pane = self.pane_mut(id).ok_or(LayoutError::UnknownPane)?;
        let from = WayBack {
            host: pane.host,
            surface: pane.surface,
        };
        pane.host = host;
        pane.surface = surface.or_default_for(host);
        if !single {
            self.way_back = None;
        } else if from.host != host {
            self.way_back = Some(from);
        }
        Ok(())
    }

    /// Returns the single pane to where it was followed from. False when there
    /// is no way back, in which case Back leaves the workspace.
    pub fn go_back(&mut self) -> bool {
        let Some(way_back) = self.way_back.take() else {
            return false;
        };
        match &mut self.arrangement {
            Arrangement::Single(pane) => {
                pane.host = way_back.host;
                pane.surface = way_back.surface.or_default_for(way_back.host);
                true
            }
            Arrangement::Split { .. } => false,
        }
    }

    /// Shows `host` without changing any pane already there.
    ///
    /// The first pane in reading order that shows `host` is reused. Otherwise a
    /// pane on `surface` is added in the smallest step the shapes allow: beside
    /// a single pane, or below a full-height pane of a split. The right column
    /// is tried first unless `from` is in it, so the new pane sits beside the
    /// pane that asked. Every existing pane keeps its host, surface, column,
    /// and row.
    ///
    /// # Errors
    ///
    /// [`LayoutError::NoRoom`] when no pane can be added without taking one
    /// away, such as in 2:2. The layout is then unchanged.
    pub fn reveal(
        &mut self,
        host: InstalledHost,
        surface: Surface,
        from: Option<PaneId>,
    ) -> Result<Revealed, LayoutError> {
        if let Some(shown) = self
            .panes()
            .into_iter()
            .find(|placed| placed.pane.host == host)
        {
            return Ok(Revealed::Shown(shown.pane.id));
        }
        let added = match self.arrangement {
            Arrangement::Single(pane) => self.split_beside(pane.id)?,
            Arrangement::Split { left, right } => {
                let (first, second) = if from.is_some_and(|id| right.contains(id)) {
                    (left, right)
                } else {
                    (right, left)
                };
                let below = first
                    .whole()
                    .or_else(|| second.whole())
                    .ok_or(LayoutError::NoRoom)?;
                self.split_below(below)?
            }
        };
        if let Some(pane) = self.pane_mut(added) {
            pane.host = host;
            pane.surface = surface.or_default_for(host);
        }
        Ok(Revealed::Added(added))
    }

    fn new_pane(&mut self, host: InstalledHost) -> Pane {
        let id = PaneId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        Pane {
            id,
            host,
            surface: Surface::DEFAULT,
        }
    }

    fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        match &mut self.arrangement {
            Arrangement::Single(pane) => (pane.id == id).then_some(pane),
            Arrangement::Split { left, right } => {
                if let Some(pane) = left.pane_mut(id) {
                    Some(pane)
                } else {
                    right.pane_mut(id)
                }
            }
        }
    }
}

/// What remains of `column` once `id` is gone, or `None` when it was the
/// column's only pane.
fn without(column: Column, id: PaneId) -> Option<Column> {
    match column {
        Column::Whole(pane) => (pane.id != id).then_some(column),
        Column::Stacked { top, bottom } if top.id == id => Some(Column::Whole(bottom)),
        Column::Stacked { top, bottom } if bottom.id == id => Some(Column::Whole(top)),
        Column::Stacked { .. } => Some(column),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use ephervex_model::{InstanceId, ServerId};

    fn client() -> InstalledHost {
        InstalledHost::Instance(InstanceId::new())
    }

    fn server() -> InstalledHost {
        InstalledHost::Server(ServerId::new())
    }

    /// Each pane's id and column and row, in reading order.
    fn places(layout: &WorkspaceLayout) -> Vec<(u32, u8, u8)> {
        layout
            .panes()
            .iter()
            .map(|placed| {
                (
                    placed.pane.id.get(),
                    placed.placement.column,
                    placed.placement.row,
                )
            })
            .collect()
    }

    fn ids(layout: &WorkspaceLayout) -> Vec<u32> {
        layout
            .panes()
            .iter()
            .map(|placed| placed.pane.id.get())
            .collect()
    }

    fn one_by_one() -> WorkspaceLayout {
        let mut layout = WorkspaceLayout::single(client(), Surface::Overview);
        layout.split_beside(PaneId(1)).expect("split beside");
        layout
    }

    fn two_by_one() -> WorkspaceLayout {
        let mut layout = one_by_one();
        layout.split_below(PaneId(1)).expect("split the left pane");
        layout
    }

    fn one_by_two() -> WorkspaceLayout {
        let mut layout = one_by_one();
        layout.split_below(PaneId(2)).expect("split the right pane");
        layout
    }

    fn two_by_two() -> WorkspaceLayout {
        let mut layout = two_by_one();
        layout.split_below(PaneId(2)).expect("split the right pane");
        layout
    }

    fn assert_supported(layout: &WorkspaceLayout) {
        let panes = layout.panes();
        assert_eq!(panes.len(), layout.shape().pane_count());
        let unique = panes
            .iter()
            .map(|placed| placed.pane.id)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), panes.len(), "pane ids are unique");
        for placed in &panes {
            assert!(placed.pane.surface.is_offered_by(placed.pane.host));
        }
    }

    #[test]
    fn a_new_workspace_is_one_pane_on_the_surface_asked_for() {
        let host = server();
        let layout = WorkspaceLayout::single(host, Surface::Console);
        assert_eq!(layout.shape(), Shape::Single);
        assert_eq!(places(&layout), [(1, 0, 0)]);
        let pane = layout.pane(PaneId(1)).expect("pane");
        assert_eq!(pane.host(), host);
        assert_eq!(pane.surface(), Surface::Console);
    }

    #[test]
    fn a_new_workspace_on_a_surface_its_host_lacks_opens_on_the_default() {
        let layout = WorkspaceLayout::single(client(), Surface::Console);
        assert_eq!(
            layout.pane(PaneId(1)).expect("pane").surface(),
            Surface::Overview
        );
    }

    #[test]
    fn each_supported_shape_places_its_panes_in_reading_order() {
        let cases = [
            (one_by_one(), Shape::OneByOne, vec![(1, 0, 0), (2, 1, 0)]),
            (
                two_by_one(),
                Shape::TwoByOne,
                vec![(1, 0, 0), (3, 0, 1), (2, 1, 0)],
            ),
            (
                one_by_two(),
                Shape::OneByTwo,
                vec![(1, 0, 0), (2, 1, 0), (3, 1, 1)],
            ),
            (
                two_by_two(),
                Shape::TwoByTwo,
                vec![(1, 0, 0), (3, 0, 1), (2, 1, 0), (4, 1, 1)],
            ),
        ];
        for (layout, shape, expected) in cases {
            assert_eq!(layout.shape(), shape);
            assert_eq!(places(&layout), expected, "{shape:?}");
            assert_supported(&layout);
        }
    }

    #[test]
    fn a_stacked_column_marks_both_of_its_panes() {
        let layout = two_by_one();
        let stacked = layout
            .panes()
            .iter()
            .map(|placed| placed.placement.stacked)
            .collect::<Vec<_>>();
        assert_eq!(stacked, [true, true, false]);
    }

    #[test]
    fn a_split_shows_the_same_host_on_its_default_surface() {
        let host = server();
        let mut layout = WorkspaceLayout::single(host, Surface::Console);
        let added = layout.split_beside(PaneId(1)).expect("split");
        let pane = layout.pane(added).expect("added pane");
        assert_eq!(pane.host(), host);
        assert_eq!(pane.surface(), Surface::Overview);
        assert_eq!(
            layout.pane(PaneId(1)).expect("source").surface(),
            Surface::Console,
            "the pane that was split keeps what it showed"
        );
    }

    #[test]
    fn only_a_single_pane_splits_beside() {
        let mut single = WorkspaceLayout::single(client(), Surface::Overview);
        assert!(single.can_split_beside(PaneId(1)));
        assert_eq!(single.split_below(PaneId(1)), Err(LayoutError::NoRoom));
        assert_eq!(single.shape(), Shape::Single);

        for mut layout in [one_by_one(), two_by_one(), one_by_two(), two_by_two()] {
            let before = layout.clone();
            for placed in before.panes() {
                assert!(!layout.can_split_beside(placed.pane.id));
                assert_eq!(
                    layout.split_beside(placed.pane.id),
                    Err(LayoutError::NoRoom)
                );
            }
            assert_eq!(layout, before, "a refused split changes nothing");
        }
    }

    #[test]
    fn a_pane_alone_in_its_column_splits_below() {
        let expectations = [
            (one_by_one(), vec![(1, true), (2, true)]),
            (two_by_one(), vec![(1, false), (3, false), (2, true)]),
            (one_by_two(), vec![(1, true), (2, false), (3, false)]),
            (
                two_by_two(),
                vec![(1, false), (3, false), (2, false), (4, false)],
            ),
        ];
        for (layout, expected) in expectations {
            let actual = layout
                .panes()
                .iter()
                .map(|placed| (placed.pane.id.get(), layout.can_split_below(placed.pane.id)))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "{:?}", layout.shape());
        }
    }

    #[test]
    fn splitting_below_reaches_every_stacked_shape() {
        let mut layout = one_by_one();
        layout.split_below(PaneId(1)).expect("left");
        assert_eq!(layout.shape(), Shape::TwoByOne);
        layout.split_below(PaneId(2)).expect("right");
        assert_eq!(layout.shape(), Shape::TwoByTwo);

        let mut layout = one_by_one();
        layout.split_below(PaneId(2)).expect("right");
        assert_eq!(layout.shape(), Shape::OneByTwo);
        layout.split_below(PaneId(1)).expect("left");
        assert_eq!(layout.shape(), Shape::TwoByTwo);
        assert_eq!(
            places(&layout),
            [(1, 0, 0), (4, 0, 1), (2, 1, 0), (3, 1, 1)]
        );
    }

    #[test]
    fn four_panes_is_the_limit() {
        let mut layout = two_by_two();
        for placed in layout.clone().panes() {
            assert_eq!(layout.split_below(placed.pane.id), Err(LayoutError::NoRoom));
            assert_eq!(
                layout.split_beside(placed.pane.id),
                Err(LayoutError::NoRoom)
            );
        }
        assert_eq!(layout.panes().len(), 4);
    }

    #[test]
    fn the_single_pane_cannot_be_closed() {
        let mut layout = WorkspaceLayout::single(client(), Surface::Overview);
        assert!(!layout.can_close(PaneId(1)));
        assert_eq!(layout.close(PaneId(1)), Err(LayoutError::LastPane));
        assert_eq!(layout.shape(), Shape::Single);
    }

    /// A layout, the pane closed in it, and the shape and places that remain.
    type CloseCase = (WorkspaceLayout, u32, Shape, Vec<(u32, u8, u8)>);

    /// Every pane of every shape, closed in turn, and what remains.
    #[test]
    fn closing_any_pane_settles_on_a_supported_shape() {
        let cases: [CloseCase; 13] = [
            (one_by_one(), 1, Shape::Single, vec![(2, 0, 0)]),
            (one_by_one(), 2, Shape::Single, vec![(1, 0, 0)]),
            // A left pane of 2:1 leaves 1:1.
            (two_by_one(), 1, Shape::OneByOne, vec![(3, 0, 0), (2, 1, 0)]),
            (two_by_one(), 3, Shape::OneByOne, vec![(1, 0, 0), (2, 1, 0)]),
            // Its right pane leaves the left stack side by side.
            (two_by_one(), 2, Shape::OneByOne, vec![(1, 0, 0), (3, 1, 0)]),
            // A right pane of 1:2 leaves 1:1.
            (one_by_two(), 2, Shape::OneByOne, vec![(1, 0, 0), (3, 1, 0)]),
            (one_by_two(), 3, Shape::OneByOne, vec![(1, 0, 0), (2, 1, 0)]),
            (one_by_two(), 1, Shape::OneByOne, vec![(2, 0, 0), (3, 1, 0)]),
            // A left pane of 2:2 leaves 1:2, a right one 2:1.
            (
                two_by_two(),
                1,
                Shape::OneByTwo,
                vec![(3, 0, 0), (2, 1, 0), (4, 1, 1)],
            ),
            (
                two_by_two(),
                3,
                Shape::OneByTwo,
                vec![(1, 0, 0), (2, 1, 0), (4, 1, 1)],
            ),
            (
                two_by_two(),
                2,
                Shape::TwoByOne,
                vec![(1, 0, 0), (3, 0, 1), (4, 1, 0)],
            ),
            (
                two_by_two(),
                4,
                Shape::TwoByOne,
                vec![(1, 0, 0), (3, 0, 1), (2, 1, 0)],
            ),
            (
                WorkspaceLayout::single(client(), Surface::Overview),
                7,
                Shape::Single,
                vec![(1, 0, 0)],
            ),
        ];
        for (mut layout, closed, shape, expected) in cases {
            let before = layout.clone();
            let result = layout.close(PaneId(closed));
            if before.pane(PaneId(closed)).is_none() {
                assert_eq!(result, Err(LayoutError::UnknownPane));
                assert_eq!(layout, before);
                continue;
            }
            result.expect("close");
            assert_eq!(
                layout.shape(),
                shape,
                "closing {closed} of {:?}",
                before.shape()
            );
            assert_eq!(
                places(&layout),
                expected,
                "closing {closed} of {:?}",
                before.shape()
            );
            assert_supported(&layout);
        }
    }

    #[test]
    fn closing_keeps_what_every_surviving_pane_shows() {
        let host = server();
        let mut layout = WorkspaceLayout::single(client(), Surface::Mods);
        layout.split_beside(PaneId(1)).expect("split");
        layout.show_host(PaneId(2), host).expect("host");
        layout
            .show_surface(PaneId(2), Surface::Console)
            .expect("surface");
        layout.split_below(PaneId(1)).expect("split");
        layout
            .show_surface(PaneId(3), Surface::Logs)
            .expect("surface");
        let survivors = |layout: &WorkspaceLayout| {
            layout
                .panes()
                .iter()
                .map(|placed| (placed.pane.id.get(), placed.pane.surface))
                .collect::<Vec<_>>()
        };

        layout.close(PaneId(1)).expect("close");
        assert_eq!(
            survivors(&layout),
            [(3, Surface::Logs), (2, Surface::Console)]
        );
        assert_eq!(layout.pane(PaneId(2)).expect("pane").host(), host);
    }

    #[test]
    fn no_sequence_of_operations_leaves_the_supported_shapes() {
        let mut layout = WorkspaceLayout::single(client(), Surface::Overview);
        // A fixed pseudo-random walk over every operation.
        let mut seed = 0x2545_f491_u32;
        for _ in 0..2_000 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let panes = layout.panes();
            let target = panes[usize::try_from(seed).unwrap_or(0) % panes.len()]
                .pane
                .id;
            let _ = match seed % 5 {
                0 => layout.split_beside(target).map(|_| ()),
                1 => layout.split_below(target).map(|_| ()),
                2 => layout.close(target),
                3 => layout.show_host(target, if seed % 2 == 0 { client() } else { server() }),
                _ => layout.show_surface(target, Surface::Console),
            };
            assert_supported(&layout);
        }
    }

    #[test]
    fn the_same_host_can_be_shown_in_several_panes_on_different_surfaces() {
        let host = client();
        let mut layout = WorkspaceLayout::single(host, Surface::Isolation);
        let logs = layout.split_beside(PaneId(1)).expect("split");
        layout.show_surface(logs, Surface::Logs).expect("surface");

        assert_eq!(layout.hosts(), [host], "one host, shown twice");
        assert_eq!(
            layout.pane(PaneId(1)).expect("pane").surface(),
            Surface::Isolation
        );
        assert_eq!(layout.pane(logs).expect("pane").surface(), Surface::Logs);
    }

    #[test]
    fn changing_one_panes_surface_leaves_every_other_pane_alone() {
        let mut layout = two_by_two();
        let before = layout.panes();
        layout
            .show_surface(PaneId(3), Surface::Snapshots)
            .expect("surface");
        for (old, new) in before.iter().zip(layout.panes()) {
            if new.pane.id == PaneId(3) {
                assert_eq!(new.pane.surface, Surface::Snapshots);
            } else {
                assert_eq!(*old, new);
            }
        }
    }

    #[test]
    fn changing_one_panes_host_leaves_every_other_pane_alone() {
        let mut layout = two_by_two();
        let before = layout.panes();
        let other = server();
        layout.show_host(PaneId(4), other).expect("host");
        for (old, new) in before.iter().zip(layout.panes()) {
            if new.pane.id == PaneId(4) {
                assert_eq!(new.pane.host, other);
            } else {
                assert_eq!(*old, new);
            }
        }
    }

    #[test]
    fn a_surface_the_host_lacks_is_refused() {
        let mut layout = WorkspaceLayout::single(client(), Surface::Mods);
        assert_eq!(
            layout.show_surface(PaneId(1), Surface::Console),
            Err(LayoutError::SurfaceNotOffered)
        );
        assert_eq!(
            layout.show_surface(PaneId(1), Surface::Properties),
            Err(LayoutError::SurfaceNotOffered)
        );
        assert_eq!(
            layout.pane(PaneId(1)).expect("pane").surface(),
            Surface::Mods
        );
        assert_eq!(
            layout.show_surface(PaneId(9), Surface::Mods),
            Err(LayoutError::UnknownPane)
        );
    }

    #[test]
    fn switching_host_kind_keeps_a_shared_surface_and_falls_back_from_a_missing_one() {
        let shared = [
            Surface::Overview,
            Surface::Mods,
            Surface::Isolation,
            Surface::Settings,
            Surface::Snapshots,
            Surface::Logs,
            Surface::Diagnostics,
        ];
        for surface in shared {
            let mut layout = WorkspaceLayout::single(client(), surface);
            layout.show_host(PaneId(1), server()).expect("host");
            assert_eq!(layout.pane(PaneId(1)).expect("pane").surface(), surface);
            layout.show_host(PaneId(1), client()).expect("host");
            assert_eq!(layout.pane(PaneId(1)).expect("pane").surface(), surface);
        }

        for surface in [Surface::Console, Surface::Properties] {
            let mut layout = WorkspaceLayout::single(server(), surface);
            layout.show_host(PaneId(1), client()).expect("host");
            assert_eq!(
                layout.pane(PaneId(1)).expect("pane").surface(),
                Surface::Overview
            );
        }
    }

    #[test]
    fn every_surface_a_host_offers_can_be_shown() {
        for host in [client(), server()] {
            let mut layout = WorkspaceLayout::single(host, Surface::Overview);
            for &surface in Surface::offered_by(host) {
                layout.show_surface(PaneId(1), surface).expect("offered");
                assert_eq!(Surface::parse(surface.id()), Some(surface));
            }
        }
        assert_eq!(Surface::parse("Nothing"), None);
    }

    #[test]
    fn following_a_link_from_a_single_pane_remembers_the_way_back() {
        let owner = client();
        let linked = server();
        let mut layout = WorkspaceLayout::single(owner, Surface::Overview);
        layout
            .follow(PaneId(1), linked, Surface::Console)
            .expect("follow");
        assert_eq!(
            layout.way_back(),
            Some(WayBack {
                host: owner,
                surface: Surface::Overview
            })
        );
        assert!(layout.go_back());
        let pane = layout.pane(PaneId(1)).expect("pane");
        assert_eq!((pane.host(), pane.surface()), (owner, Surface::Overview));
        assert!(!layout.go_back(), "the way back is used once");
    }

    #[test]
    fn a_split_forgets_the_way_back_and_follows_links_in_place() {
        let owner = client();
        let linked = server();
        let mut layout = WorkspaceLayout::single(owner, Surface::Overview);
        layout
            .follow(PaneId(1), linked, Surface::Overview)
            .expect("follow");
        let added = layout.split_beside(PaneId(1)).expect("split");
        assert_eq!(layout.way_back(), None);

        layout.follow(added, owner, Surface::Mods).expect("follow");
        assert_eq!(layout.way_back(), None);
        assert!(!layout.go_back());
        assert_eq!(layout.pane(added).expect("pane").host(), owner);
        assert_eq!(layout.pane(PaneId(1)).expect("pane").host(), linked);
    }

    #[test]
    fn pane_ids_are_never_reused() {
        let mut layout = one_by_one();
        layout.close(PaneId(2)).expect("close");
        let added = layout.split_beside(PaneId(1)).expect("split");
        assert_eq!(added, PaneId(3));
        assert_eq!(ids(&layout), [1, 3]);
    }

    /// Every pane's id, host, surface, column, and row, in reading order.
    fn contents(layout: &WorkspaceLayout) -> Vec<(u32, InstalledHost, Surface, u8, u8)> {
        layout
            .panes()
            .iter()
            .map(|placed| {
                (
                    placed.pane.id.get(),
                    placed.pane.host,
                    placed.pane.surface,
                    placed.placement.column,
                    placed.placement.row,
                )
            })
            .collect()
    }

    /// Each shape with a different host and a non-default surface in every pane.
    fn arranged(shape: Shape) -> WorkspaceLayout {
        let mut layout = WorkspaceLayout::single(client(), Surface::Mods);
        if shape == Shape::Single {
            return layout;
        }
        layout.split_beside(PaneId(1)).expect("beside");
        layout.show_host(PaneId(2), server()).expect("host");
        layout
            .show_surface(PaneId(2), Surface::Console)
            .expect("surface");
        let stack = |layout: &mut WorkspaceLayout, below: u32, surface: Surface| {
            let added = layout.split_below(PaneId(below)).expect("below");
            layout.show_host(added, client()).expect("host");
            layout.show_surface(added, surface).expect("surface");
        };
        match shape {
            Shape::TwoByOne => stack(&mut layout, 1, Surface::Logs),
            Shape::OneByTwo => stack(&mut layout, 2, Surface::Snapshots),
            Shape::TwoByTwo => {
                stack(&mut layout, 1, Surface::Logs);
                stack(&mut layout, 2, Surface::Snapshots);
            }
            Shape::Single | Shape::OneByOne => {}
        }
        assert_eq!(layout.shape(), shape);
        layout
    }

    const SHAPES: [Shape; 5] = [
        Shape::Single,
        Shape::OneByOne,
        Shape::TwoByOne,
        Shape::OneByTwo,
        Shape::TwoByTwo,
    ];

    fn added(revealed: Result<Revealed, LayoutError>) -> Option<PaneId> {
        match revealed {
            Ok(Revealed::Added(pane)) => Some(pane),
            _ => None,
        }
    }

    /// Reveals `host` and checks that every pane already there is unchanged.
    fn reveal_keeping_every_pane(
        layout: &mut WorkspaceLayout,
        host: InstalledHost,
        from: Option<PaneId>,
    ) -> PaneId {
        let before = contents(layout);
        let added =
            added(layout.reveal(host, Surface::Console, from)).expect("room for one more pane");
        let after = contents(layout);
        assert_eq!(after.len(), before.len() + 1);
        for pane in &before {
            assert!(after.contains(pane), "{pane:?} is untouched");
        }
        let pane = layout.pane(added).expect("added pane");
        assert_eq!(pane.host(), host);
        assert_supported(layout);
        added
    }

    #[test]
    fn revealing_a_host_in_a_single_pane_adds_it_beside() {
        let mut layout = arranged(Shape::Single);
        let host = server();
        let added = reveal_keeping_every_pane(&mut layout, host, Some(PaneId(1)));
        assert_eq!(layout.shape(), Shape::OneByOne);
        assert_eq!(
            contents(&layout)[1],
            (added.get(), host, Surface::Console, 1, 0)
        );
    }

    #[test]
    fn revealing_a_host_in_1_1_stacks_it_beside_the_pane_that_asked() {
        let host = server();
        let mut layout = arranged(Shape::OneByOne);
        let added = reveal_keeping_every_pane(&mut layout, host, Some(PaneId(1)));
        assert_eq!(layout.shape(), Shape::OneByTwo);
        assert_eq!(
            contents(&layout)[2],
            (added.get(), host, Surface::Console, 1, 1)
        );

        let mut layout = arranged(Shape::OneByOne);
        let added = reveal_keeping_every_pane(&mut layout, host, Some(PaneId(2)));
        assert_eq!(layout.shape(), Shape::TwoByOne);
        assert_eq!(
            contents(&layout)[1],
            (added.get(), host, Surface::Console, 0, 1)
        );

        let mut layout = arranged(Shape::OneByOne);
        reveal_keeping_every_pane(&mut layout, host, None);
        assert_eq!(layout.shape(), Shape::OneByTwo);
    }

    #[test]
    fn revealing_a_host_in_2_1_fills_the_right_column() {
        let host = server();
        for from in [Some(PaneId(1)), Some(PaneId(2)), Some(PaneId(3)), None] {
            let mut layout = arranged(Shape::TwoByOne);
            let added = reveal_keeping_every_pane(&mut layout, host, from);
            assert_eq!(layout.shape(), Shape::TwoByTwo);
            assert_eq!(
                contents(&layout)[3],
                (added.get(), host, Surface::Console, 1, 1)
            );
        }
    }

    #[test]
    fn revealing_a_host_in_1_2_fills_the_left_column() {
        let host = server();
        for from in [Some(PaneId(1)), Some(PaneId(2)), Some(PaneId(3)), None] {
            let mut layout = arranged(Shape::OneByTwo);
            let added = reveal_keeping_every_pane(&mut layout, host, from);
            assert_eq!(layout.shape(), Shape::TwoByTwo);
            assert_eq!(
                contents(&layout)[1],
                (added.get(), host, Surface::Console, 0, 1)
            );
        }
    }

    #[test]
    fn a_full_workspace_is_left_exactly_as_it_is() {
        let mut layout = arranged(Shape::TwoByTwo);
        let before = layout.clone();
        for from in [None, Some(PaneId(1)), Some(PaneId(4))] {
            assert_eq!(
                layout.reveal(server(), Surface::Console, from),
                Err(LayoutError::NoRoom)
            );
            assert_eq!(layout, before);
        }
    }

    #[test]
    fn a_host_already_shown_is_revealed_where_it_is_and_nothing_changes() {
        for shape in SHAPES {
            let layout = arranged(shape);
            for placed in layout.panes() {
                let mut revealed = layout.clone();
                let first = layout
                    .panes()
                    .into_iter()
                    .find(|other| other.pane.host == placed.pane.host)
                    .expect("shown")
                    .pane
                    .id;
                assert_eq!(
                    revealed.reveal(placed.pane.host, Surface::Overview, Some(placed.pane.id)),
                    Ok(Revealed::Shown(first)),
                    "{shape:?}"
                );
                assert_eq!(revealed, layout, "{shape:?}");
            }
        }
    }

    #[test]
    fn a_host_shown_twice_gets_no_further_pane() {
        let host = server();
        let mut layout = WorkspaceLayout::single(host, Surface::Console);
        let logs = layout.split_beside(PaneId(1)).expect("split");
        layout.show_surface(logs, Surface::Logs).expect("surface");
        let before = layout.clone();

        assert_eq!(
            layout.reveal(host, Surface::Overview, Some(logs)),
            Ok(Revealed::Shown(PaneId(1)))
        );
        assert_eq!(layout, before);
    }

    #[test]
    fn revealing_the_same_host_again_adds_nothing_more() {
        for shape in [
            Shape::Single,
            Shape::OneByOne,
            Shape::TwoByOne,
            Shape::OneByTwo,
        ] {
            let mut layout = arranged(shape);
            let host = server();
            let added = reveal_keeping_every_pane(&mut layout, host, Some(PaneId(1)));
            let once = layout.clone();
            assert_eq!(
                layout.reveal(host, Surface::Console, Some(PaneId(1))),
                Ok(Revealed::Shown(added))
            );
            assert_eq!(layout, once, "{shape:?}");
        }
    }

    #[test]
    fn a_revealed_host_opens_on_a_surface_it_offers() {
        let mut layout = WorkspaceLayout::single(server(), Surface::Console);
        let host = client();
        let added =
            added(layout.reveal(host, Surface::Console, None)).expect("a single pane has room");
        assert_eq!(
            layout.pane(added).expect("pane").surface(),
            Surface::Overview
        );
    }
}
