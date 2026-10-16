//! What the Instances page shows: search, filters and order over the rows the
//! newest snapshot described (ADR 0048).
//!
//! The window holds each filter as a position in an option list this module
//! writes. Everything that decides what matches and in what order is core's, so
//! the list agrees with `ephervex instances list`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ephervex_core::{
    GroupFilter, InstanceFacts, InstanceFilter, InstanceGroup, InstanceSort, LinkingState,
    RunFilter, SearchQuery, VersionOrder,
};
use ephervex_i18n::lookup;
use ephervex_model::{LoaderKind, StorageClass};
use slint::{ComponentHandle as _, Image, Model as _, ModelRc, SharedString, VecModel};

use super::{attach_creating_servers, model, reconciled, to_i32};
use crate::ui::{AppWindow, GroupRow, HostIcons, InstanceRow};

/// The images icons are drawn from, which rows name by position.
struct IconPool {
    images: Rc<VecModel<Image>>,
    slots: HashMap<PathBuf, i32>,
}

thread_local! {
    /// A stored icon is named by its content, so a path never changes what it
    /// shows and a loaded image never needs replacing.
    static ICONS: RefCell<IconPool> = RefCell::new(IconPool {
        images: Rc::new(VecModel::default()),
        slots: HashMap::new(),
    });
}

/// Hands the window the pool its rows' `icon-slot` values point into.
pub(super) fn attach_icon_pool(ui: &AppWindow) {
    ICONS.with(|pool| {
        ui.global::<HostIcons>()
            .set_images(ModelRc::from(Rc::clone(&pool.borrow().images)));
    });
}

/// Where the image at `path` is in the pool, or -1 when there is none or it
/// will not load, which the markup draws as the loader glyph.
pub(super) fn host_icon_slot(path: Option<&Path>) -> i32 {
    let Some(path) = path else {
        return -1;
    };
    ICONS.with(|pool| {
        let mut pool = pool.borrow_mut();
        if let Some(slot) = pool.slots.get(path) {
            return *slot;
        }
        // A file that is gone is no icon, and is asked about again next time:
        // the same icon can be stored again. One that is there and will not
        // load is remembered, so it is not retried and reported on every
        // refresh.
        if !path.is_file() {
            return -1;
        }
        let Ok(image) = Image::load_from_path(path) else {
            pool.slots.insert(path.to_path_buf(), -1);
            return -1;
        };
        let slot = to_i32(pool.images.row_count());
        pool.images.push(image);
        pool.slots.insert(path.to_path_buf(), slot);
        slot
    })
}

const LOADERS: [Option<LoaderKind>; 4] = [
    None,
    Some(LoaderKind::Vanilla),
    Some(LoaderKind::Forge),
    Some(LoaderKind::NeoForge),
];
const RUN_STATES: [Option<RunFilter>; 3] =
    [None, Some(RunFilter::Running), Some(RunFilter::Stopped)];
const LINKINGS: [Option<LinkingState>; 4] = [
    None,
    Some(LinkingState::Paired),
    Some(LinkingState::LinkedServers),
    Some(LinkingState::None),
];

/// One instance as the list holds it: its row, and what search, filters and
/// order read.
#[derive(Debug, Clone)]
pub(super) struct Listed {
    pub row: InstanceRow,
    pub facts: InstanceFacts,
}

/// The option labels last written to the window, so an edit that does not
/// change them leaves an open dropdown alone.
#[derive(Default)]
struct WrittenOptions {
    minecraft: Vec<String>,
    loader: Vec<String>,
    run: Vec<String>,
    linking: Vec<String>,
    group: Vec<String>,
    sort: Vec<String>,
}

#[derive(Default)]
pub(super) struct InstanceListState {
    listed: Vec<Listed>,
    groups: Vec<InstanceGroup>,
    sort: InstanceSort,
    order: VersionOrder,
    /// The Minecraft version each option stands for. Position 0 is "any".
    minecraft_options: Vec<String>,
    /// Position 0 is any group and position 1 is ungrouped.
    group_options: Vec<GroupFilter>,
    written: WrittenOptions,
}

/// Which row is selected after the list changed.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Selection {
    Row(usize),
    /// The instance exists but the filters hide it. The selection stays with
    /// it, and nothing else is selected in its place.
    Hidden,
    Nothing,
}

/// Keeps the selection by id across a re-sort or a re-filter.
pub(super) fn selection(rows: &[InstanceRow], previous: &str, exists: bool) -> Selection {
    let first = || {
        if rows.is_empty() {
            Selection::Nothing
        } else {
            Selection::Row(0)
        }
    };
    if previous.is_empty() {
        return first();
    }
    match rows.iter().position(|row| row.id == previous) {
        Some(index) => Selection::Row(index),
        None if exists => Selection::Hidden,
        None => first(),
    }
}

fn labels(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| lookup(id)).collect()
}

fn push_options(
    ui: &AppWindow,
    written: &mut Vec<String>,
    options: Vec<String>,
    write: impl Fn(&AppWindow, slint::ModelRc<SharedString>),
) {
    if *written != options {
        write(ui, model(options.iter().map(SharedString::from).collect()));
        *written = options;
    }
}

impl InstanceListState {
    /// Takes the newest instances, keeps every choice that still exists, and
    /// shows the list. Returns every instance's row in storage order, before
    /// any filter, which is what the workspace is given.
    pub(super) fn replace(
        &mut self,
        ui: &AppWindow,
        listed: Vec<Listed>,
        groups: Vec<InstanceGroup>,
        sort: InstanceSort,
        order: VersionOrder,
    ) -> Vec<InstanceRow> {
        let carried_minecraft = usize::try_from(ui.get_instance_minecraft_index())
            .ok()
            .and_then(|index| self.minecraft_options.get(index))
            .cloned();
        let carried_group = usize::try_from(ui.get_instance_group_index())
            .ok()
            .and_then(|index| self.group_options.get(index))
            .copied();

        self.listed = listed;
        self.groups = groups;
        self.sort = sort;
        self.order = order;

        let mut versions: Vec<String> = self
            .listed
            .iter()
            .map(|listed| listed.facts.minecraft_version.clone())
            .collect();
        versions.sort_by(|a, b| self.order.compare_newest_first(a, b));
        versions.dedup();
        self.minecraft_options = std::iter::once(String::new()).chain(versions).collect();
        self.group_options = [GroupFilter::Any, GroupFilter::Ungrouped]
            .into_iter()
            .chain(self.groups.iter().map(|group| GroupFilter::Group(group.id)))
            .collect();

        let minecraft_index = carried_minecraft
            .and_then(|version| self.minecraft_options.iter().position(|id| *id == version))
            .unwrap_or(0);
        let group_index = carried_group
            .and_then(|group| {
                self.group_options
                    .iter()
                    .position(|option| *option == group)
            })
            .unwrap_or(0);
        ui.set_instance_minecraft_index(to_i32(minecraft_index));
        ui.set_instance_group_index(to_i32(group_index));

        self.write_options(ui);
        ui.set_instance_groups(model(
            self.groups
                .iter()
                .map(|group| GroupRow {
                    id: group.id.to_string().into(),
                    name: group.name.as_str().into(),
                    tone: i32::from(group.tone),
                    members: to_i32(
                        self.listed
                            .iter()
                            .filter(|listed| {
                                listed
                                    .facts
                                    .group
                                    .as_ref()
                                    .is_some_and(|g| g.id == group.id)
                            })
                            .count(),
                    ),
                })
                .collect(),
        ));
        self.render(ui);
        self.listed
            .iter()
            .map(|listed| listed.row.clone())
            .collect()
    }

    fn write_options(&mut self, ui: &AppWindow) {
        let mut minecraft = vec![lookup("gui-filter-any-version")];
        minecraft.extend(self.minecraft_options.iter().skip(1).cloned());
        push_options(ui, &mut self.written.minecraft, minecraft, |ui, options| {
            ui.set_instance_minecraft_options(options);
        });
        push_options(
            ui,
            &mut self.written.loader,
            labels(&[
                "gui-filter-any-loader",
                "gui-loader-vanilla",
                "gui-loader-forge",
                "gui-loader-neoforge",
            ]),
            AppWindow::set_instance_loader_options,
        );
        push_options(
            ui,
            &mut self.written.run,
            labels(&[
                "gui-filter-any-run-state",
                "gui-filter-running",
                "gui-filter-stopped",
            ]),
            AppWindow::set_instance_run_options,
        );
        push_options(
            ui,
            &mut self.written.linking,
            labels(&[
                "gui-filter-any-linking",
                "gui-filter-linking-paired",
                "gui-filter-linking-servers",
                "gui-filter-linking-none",
            ]),
            AppWindow::set_instance_linking_options,
        );
        let mut groups = labels(&["gui-filter-any-group", "gui-filter-ungrouped"]);
        groups.extend(self.groups.iter().map(|group| group.name.clone()));
        push_options(ui, &mut self.written.group, groups, |ui, options| {
            ui.set_instance_group_options(options);
        });
        push_options(
            ui,
            &mut self.written.sort,
            labels(&[
                "gui-sort-favorites",
                "gui-sort-name",
                "gui-sort-minecraft",
                "gui-sort-loader",
            ]),
            AppWindow::set_instance_sort_options,
        );
        ui.set_instance_group_names(model(
            self.groups
                .iter()
                .map(|group| SharedString::from(group.name.as_str()))
                .collect(),
        ));
        ui.set_instance_sort_index(to_i32(
            InstanceSort::ALL
                .iter()
                .position(|sort| *sort == self.sort)
                .unwrap_or(0),
        ));
    }

    /// What the window's controls currently ask for.
    fn filter(&self, ui: &AppWindow) -> InstanceFilter {
        let pick = |index: i32| usize::try_from(index).unwrap_or(0);
        InstanceFilter {
            query: ui.get_instance_query().to_string(),
            storage: match ui.get_instance_filter().as_str() {
                "temporary" => Some(StorageClass::Temporary),
                "persistent" => Some(StorageClass::Persistent),
                _ => None,
            },
            favorites_only: ui.get_instance_favorites_only(),
            minecraft_version: self
                .minecraft_options
                .get(pick(ui.get_instance_minecraft_index()))
                .filter(|version| !version.is_empty())
                .cloned(),
            loader: LOADERS
                .get(pick(ui.get_instance_loader_index()))
                .copied()
                .flatten(),
            run_state: RUN_STATES
                .get(pick(ui.get_instance_run_index()))
                .copied()
                .flatten(),
            linking: LINKINGS
                .get(pick(ui.get_instance_linking_index()))
                .copied()
                .flatten(),
            group: self
                .group_options
                .get(pick(ui.get_instance_group_index()))
                .copied()
                .unwrap_or_default(),
        }
    }

    /// Shows the instances the controls ask for, in the chosen order.
    pub(super) fn render(&self, ui: &AppWindow) {
        let filter = self.filter(ui);
        let ordered = ephervex_core::list_instances(
            self.listed
                .iter()
                .map(|listed| listed.facts.clone())
                .collect(),
            &filter,
            self.sort,
            &self.order,
        );
        let by_id: HashMap<String, &InstanceRow> = self
            .listed
            .iter()
            .map(|listed| (listed.row.id.to_string(), &listed.row))
            .collect();
        let rows: Vec<InstanceRow> = ordered
            .iter()
            .filter_map(|facts| by_id.get(&facts.id.to_string()).map(|row| (*row).clone()))
            .collect();

        let previous = ui.get_selected_instance_id();
        let exists = by_id.contains_key(previous.as_str());
        match selection(&rows, previous.as_str(), exists) {
            Selection::Row(index) => {
                if ui.get_selected_instance_id() != rows[index].id {
                    ui.set_selected_instance_id(rows[index].id.clone());
                }
                ui.set_selected_instance_index(to_i32(index));
            }
            Selection::Hidden => ui.set_selected_instance_index(-1),
            Selection::Nothing => {
                ui.set_selected_instance_id(SharedString::default());
                ui.set_selected_instance_index(-1);
            }
        }
        let query_active = !SearchQuery::new(&filter.query).is_empty();
        ui.set_instance_total(to_i32(self.listed.len()));
        ui.set_instance_filters_active(to_i32(filter.active_count() + usize::from(query_active)));
        ui.set_instances(reconciled(&ui.get_instances(), rows));
        // A render rebuilds the rows from records that know nothing of a
        // server being built for them.
        attach_creating_servers(ui);
    }

    /// Orders the list by `sort` from now on.
    pub(super) fn set_sort(&mut self, ui: &AppWindow, sort: InstanceSort) {
        self.sort = sort;
        self.write_options(ui);
        self.render(ui);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use chrono::{DateTime, Utc};
    use ephervex_core::InstanceFacts;
    use ephervex_model::{GroupId, InstanceId, LoaderSelection};

    fn row(id: &str) -> InstanceRow {
        InstanceRow {
            id: id.into(),
            ..InstanceRow::default()
        }
    }

    fn window() -> AppWindow {
        let _ =
            slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
                i_slint_backend_testing::TestingBackendOptions {
                    threading: false,
                    ..Default::default()
                },
            )));
        AppWindow::new().expect("window")
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + seconds, 0).expect("a timestamp")
    }

    fn listed(name: &str, minecraft: &str, seconds: i64) -> Listed {
        let id = InstanceId::new();
        let facts = InstanceFacts {
            id,
            name: name.to_owned(),
            storage: StorageClass::Persistent,
            minecraft_version: minecraft.to_owned(),
            loader: LoaderSelection::vanilla(),
            source: "Manual".to_owned(),
            created_at: at(seconds),
            favorite: false,
            group: None,
            linking: LinkingState::None,
            running: false,
        };
        let row = InstanceRow {
            id: id.to_string().into(),
            name: name.into(),
            minecraft: minecraft.into(),
            ..InstanceRow::default()
        };
        Listed { row, facts }
    }

    fn favorite(mut listed: Listed) -> Listed {
        listed.facts.favorite = true;
        listed.row.favorite = true;
        listed
    }

    fn names(ui: &AppWindow) -> Vec<String> {
        ui.get_instances()
            .iter()
            .map(|row| row.name.to_string())
            .collect()
    }

    fn show(
        ui: &AppWindow,
        state: &mut InstanceListState,
        instances: Vec<Listed>,
        sort: InstanceSort,
    ) {
        state.replace(ui, instances, Vec::new(), sort, VersionOrder::default());
    }

    #[test]
    fn the_selection_follows_its_instance_to_a_new_position() {
        let rows = [row("b"), row("a")];
        assert_eq!(selection(&rows, "a", true), Selection::Row(1));
    }

    #[test]
    fn a_selection_the_filters_hide_stays_with_its_instance() {
        let rows = [row("b")];
        assert_eq!(selection(&rows, "a", true), Selection::Hidden);
    }

    #[test]
    fn a_selection_whose_instance_is_gone_moves_to_the_first_row() {
        let rows = [row("b"), row("c")];
        assert_eq!(selection(&rows, "a", false), Selection::Row(0));
        assert_eq!(selection(&[], "a", false), Selection::Nothing);
    }

    #[test]
    fn nothing_selected_starts_on_the_first_row() {
        assert_eq!(selection(&[row("a")], "", false), Selection::Row(0));
        assert_eq!(selection(&[], "", false), Selection::Nothing);
    }

    #[test]
    fn the_default_order_is_favorites_first_then_newest_first() {
        let ui = window();
        let mut state = InstanceListState::default();
        show(
            &ui,
            &mut state,
            vec![
                listed("old", "1.20.1", 0),
                favorite(listed("pinned", "1.20.1", -50)),
                listed("new", "1.20.1", 50),
            ],
            InstanceSort::default(),
        );
        assert_eq!(names(&ui), ["pinned", "new", "old"]);
    }

    #[test]
    fn a_search_applies_on_each_edit_and_matches_the_group_name() {
        let ui = window();
        let mut state = InstanceListState::default();
        let mut grouped = listed("repro", "1.20.1", 0);
        grouped.facts.group = Some(InstanceGroup {
            id: GroupId::new(),
            name: "Bisect".to_owned(),
            tone: 0,
        });
        state.replace(
            &ui,
            vec![grouped, listed("other", "1.21", 1)],
            Vec::new(),
            InstanceSort::default(),
            VersionOrder::default(),
        );
        assert_eq!(names(&ui).len(), 2);
        assert_eq!(ui.get_instance_total(), 2);

        ui.set_instance_query("BISECT".into());
        state.render(&ui);
        assert_eq!(names(&ui), ["repro"]);
        assert_eq!(ui.get_instance_filters_active(), 1);

        ui.set_instance_favorites_only(true);
        state.render(&ui);
        assert_eq!(
            ui.get_instance_filters_active(),
            2,
            "the search and one filter are two"
        );
        ui.set_instance_favorites_only(false);

        // A search of only spaces narrows nothing and is not counted.
        ui.set_instance_query("   ".into());
        state.render(&ui);
        assert_eq!(names(&ui).len(), 2);
        assert_eq!(ui.get_instance_filters_active(), 0);

        ui.set_instance_query(SharedString::default());
        state.render(&ui);
        assert_eq!(names(&ui).len(), 2);
        assert_eq!(ui.get_instance_filters_active(), 0);
    }

    #[test]
    fn filters_combine_and_each_has_an_any_position() {
        let ui = window();
        let mut state = InstanceListState::default();
        show(
            &ui,
            &mut state,
            vec![
                favorite(listed("a", "1.20.1", 0)),
                listed("b", "1.20.1", 1),
                favorite(listed("c", "1.21", 2)),
            ],
            InstanceSort::default(),
        );
        ui.set_instance_favorites_only(true);
        state.render(&ui);
        assert_eq!(names(&ui).len(), 2);

        // Position 0 is any version; the versions present follow, newest first.
        let versions: Vec<String> = ui
            .get_instance_minecraft_options()
            .iter()
            .map(|label| label.to_string())
            .collect();
        assert_eq!(&versions[1..], ["1.21", "1.20.1"]);
        ui.set_instance_minecraft_index(2);
        state.render(&ui);
        assert_eq!(names(&ui), ["a"]);
        assert_eq!(ui.get_instance_filters_active(), 2);
        assert_eq!(ui.get_instance_total(), 3);
    }

    #[test]
    fn the_selection_survives_a_re_sort_and_a_hiding_filter() {
        let ui = window();
        let mut state = InstanceListState::default();
        let wanted = listed("b", "1.20.1", 1);
        let wanted_id = wanted.row.id.clone();
        show(
            &ui,
            &mut state,
            vec![listed("a", "1.20.1", 0), wanted, listed("c", "1.20.1", 2)],
            InstanceSort::Name,
        );
        ui.set_selected_instance_id(wanted_id.clone());
        state.render(&ui);
        assert_eq!(ui.get_selected_instance_index(), 1);

        state.set_sort(&ui, InstanceSort::Favorite);
        assert_eq!(ui.get_selected_instance_id(), wanted_id);
        assert_eq!(
            usize::try_from(ui.get_selected_instance_index()).ok(),
            names(&ui).iter().position(|name| name == "b")
        );

        ui.set_instance_query("zzz".into());
        state.render(&ui);
        assert_eq!(ui.get_selected_instance_id(), wanted_id);
        assert_eq!(ui.get_selected_instance_index(), -1);
    }

    #[test]
    fn a_filter_choice_carries_over_to_the_next_snapshot_by_what_it_names() {
        let ui = window();
        let mut state = InstanceListState::default();
        show(
            &ui,
            &mut state,
            vec![listed("a", "1.20.1", 0), listed("b", "1.21", 1)],
            InstanceSort::default(),
        );
        ui.set_instance_minecraft_index(2);
        state.render(&ui);
        assert_eq!(names(&ui), ["a"]);

        // A newer release shifts every position; the choice stays on 1.20.1.
        show(
            &ui,
            &mut state,
            vec![
                listed("a", "1.20.1", 0),
                listed("b", "1.21", 1),
                listed("c", "1.21.4", 2),
            ],
            InstanceSort::default(),
        );
        assert_eq!(names(&ui), ["a"]);
        assert_eq!(ui.get_instance_minecraft_index(), 3);

        // A version no instance has any more drops back to any.
        show(
            &ui,
            &mut state,
            vec![listed("b", "1.21", 1)],
            InstanceSort::default(),
        );
        assert_eq!(ui.get_instance_minecraft_index(), 0);
        assert_eq!(names(&ui), ["b"]);
    }

    #[test]
    fn the_group_choices_are_only_real_groups_even_one_named_like_the_reset() {
        let ui = window();
        let mut state = InstanceListState::default();
        let groups: Vec<InstanceGroup> = ["No group", "Render bugs"]
            .into_iter()
            .map(|name| InstanceGroup {
                id: GroupId::new(),
                name: name.to_owned(),
                tone: 0,
            })
            .collect();
        state.replace(
            &ui,
            vec![listed("a", "1.20.1", 0)],
            groups,
            InstanceSort::default(),
            VersionOrder::default(),
        );
        let choices: Vec<String> = ui
            .get_instance_group_names()
            .iter()
            .map(|name| name.to_string())
            .collect();
        assert_eq!(choices, ["No group", "Render bugs"]);
    }

    #[test]
    fn a_group_choice_follows_its_group_through_a_rename_and_falls_back_when_deleted() {
        let ui = window();
        let mut state = InstanceListState::default();
        let group = InstanceGroup {
            id: GroupId::new(),
            name: "One".to_owned(),
            tone: 0,
        };
        let mut member = listed("in", "1.20.1", 0);
        member.facts.group = Some(group.clone());
        let others = listed("out", "1.20.1", 1);
        state.replace(
            &ui,
            vec![member.clone(), others.clone()],
            vec![group.clone()],
            InstanceSort::default(),
            VersionOrder::default(),
        );
        ui.set_instance_group_index(2);
        state.render(&ui);
        assert_eq!(names(&ui), ["in"]);

        let renamed = InstanceGroup {
            name: "Two".to_owned(),
            ..group
        };
        state.replace(
            &ui,
            vec![member.clone(), others.clone()],
            vec![renamed],
            InstanceSort::default(),
            VersionOrder::default(),
        );
        assert_eq!(names(&ui), ["in"]);

        state.replace(
            &ui,
            vec![others],
            Vec::new(),
            InstanceSort::default(),
            VersionOrder::default(),
        );
        assert_eq!(ui.get_instance_group_index(), 0);
        assert_eq!(names(&ui), ["out"]);
    }

    #[test]
    fn revealing_an_instance_clears_every_filter_so_nothing_can_hide_it() {
        let ui = window();
        ui.set_instance_filter("temporary".into());
        ui.set_instance_query("zzz".into());
        ui.set_instance_favorites_only(true);
        ui.set_instance_minecraft_index(2);
        ui.set_instance_loader_index(1);
        ui.set_instance_run_index(1);
        ui.set_instance_linking_index(1);
        ui.set_instance_group_index(2);

        super::super::reveal_instance(&ui, "wanted");

        assert_eq!(ui.get_selected_instance_id(), "wanted");
        assert_eq!(ui.get_instance_filter(), "all");
        assert_eq!(ui.get_instance_query(), "");
        assert!(!ui.get_instance_favorites_only());
        assert_eq!(ui.get_instance_minecraft_index(), 0);
        assert_eq!(ui.get_instance_loader_index(), 0);
        assert_eq!(ui.get_instance_run_index(), 0);
        assert_eq!(ui.get_instance_linking_index(), 0);
        assert_eq!(ui.get_instance_group_index(), 0);
    }

    #[test]
    fn a_row_equals_an_identical_row_so_a_refresh_leaves_it_alone() {
        // `reconciled` writes a row only where it differs. An image field would
        // never compare equal, and every snapshot would rewrite every row.
        let first = listed("a", "1.20.1", 0).row;
        assert_eq!(first, first.clone());
        assert_eq!(InstanceRow::default(), InstanceRow::default());
    }

    #[test]
    fn an_icon_that_is_missing_or_not_an_image_has_no_slot_and_a_loaded_one_is_reused() {
        let directory = tempfile::tempdir().expect("temp dir");
        assert_eq!(host_icon_slot(None), -1);
        assert_eq!(host_icon_slot(Some(&directory.path().join("gone.png"))), -1);

        let broken = directory.path().join("broken.png");
        std::fs::write(&broken, b"not a png").expect("write");
        assert_eq!(host_icon_slot(Some(&broken)), -1);

        let png = directory.path().join("icon.png");
        std::fs::write(&png, ONE_PIXEL_PNG).expect("write");
        let slot = host_icon_slot(Some(&png));
        assert!(slot >= 0);
        assert_eq!(host_icon_slot(Some(&png)), slot);
    }

    /// A valid 1x1 PNG.
    const ONE_PIXEL_PNG: [u8; 70] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xfc,
        0xcf, 0xc0, 0x50, 0x0f, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xa9, 0x8c, 0x21, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
}
