//! Searching, filtering and ordering the instance list (ADR 0048).
//!
//! Pure functions over [`InstanceFacts`], so the CLI, the desktop list and the
//! host picker agree on what matches and in what order. Nothing here reads the
//! network: Minecraft release order comes from a cached manifest when there is
//! one and from a version-aware text comparison when there is not.

use std::cmp::Ordering;
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ephervex_model::{GroupId, InstanceId, LoaderKind, LoaderSelection, StorageClass};
use ephervex_storage::ClientRecord;

use crate::linked_pair::LinkingState;
use crate::organization::InstanceGroup;

/// Everything about one instance that searching, filtering and ordering look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceFacts {
    pub id: InstanceId,
    pub name: String,
    pub storage: StorageClass,
    pub minecraft_version: String,
    pub loader: LoaderSelection,
    /// Where the instance came from, as shown: the import kind and reference.
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub favorite: bool,
    pub group: Option<InstanceGroup>,
    pub linking: LinkingState,
    /// Launching or running.
    pub running: bool,
}

impl InstanceFacts {
    /// The facts a record carries; the caller supplies what lives elsewhere.
    #[must_use]
    pub fn from_record(
        record: &ClientRecord,
        favorite: bool,
        group: Option<InstanceGroup>,
        linking: LinkingState,
        running: bool,
    ) -> Self {
        let source = match &record.source.reference {
            Some(reference) => format!("{} {reference}", record.source.kind),
            None => record.source.kind.to_string(),
        };
        Self {
            id: record.id,
            name: record.name.clone(),
            storage: record.storage,
            minecraft_version: record.minecraft_version.clone(),
            loader: record.loader.clone(),
            source,
            created_at: record.created_at,
            favorite,
            group,
            linking,
            running,
        }
    }
}

/// A search text, compared without regard to case.
///
/// Trimmed, lowercased with Unicode rules, and matched as a substring. An
/// empty search matches everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery(String);

impl SearchQuery {
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self(text.trim().to_lowercase())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether any of `fields` contains the search.
    #[must_use]
    pub fn matches_any<'a>(&self, fields: impl IntoIterator<Item = &'a str>) -> bool {
        self.0.is_empty()
            || fields
                .into_iter()
                .any(|field| field.to_lowercase().contains(&self.0))
    }
}

/// Whether to keep running or stopped instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFilter {
    /// Launching or running.
    Running,
    /// Not launching and not running.
    Stopped,
}

/// Which instances by group.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GroupFilter {
    #[default]
    Any,
    Group(GroupId),
    Ungrouped,
}

/// The combination of conditions an instance must meet to be listed.
///
/// Every condition that is set must hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstanceFilter {
    pub query: String,
    pub storage: Option<StorageClass>,
    pub favorites_only: bool,
    pub minecraft_version: Option<String>,
    pub loader: Option<LoaderKind>,
    pub run_state: Option<RunFilter>,
    pub linking: Option<LinkingState>,
    pub group: GroupFilter,
}

impl InstanceFilter {
    /// How many conditions besides the search are set.
    #[must_use]
    pub fn active_count(&self) -> usize {
        usize::from(self.storage.is_some())
            + usize::from(self.favorites_only)
            + usize::from(self.minecraft_version.is_some())
            + usize::from(self.loader.is_some())
            + usize::from(self.run_state.is_some())
            + usize::from(self.linking.is_some())
            + usize::from(self.group != GroupFilter::Any)
    }

    /// Whether anything, the search included, narrows the list.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active_count() > 0 || !SearchQuery::new(&self.query).is_empty()
    }

    #[must_use]
    pub fn matches(&self, facts: &InstanceFacts) -> bool {
        self.storage.is_none_or(|storage| storage == facts.storage)
            && (!self.favorites_only || facts.favorite)
            && self
                .minecraft_version
                .as_ref()
                .is_none_or(|version| *version == facts.minecraft_version)
            && self.loader.is_none_or(|kind| kind == facts.loader.kind)
            && self.run_state.is_none_or(|state| match state {
                RunFilter::Running => facts.running,
                RunFilter::Stopped => !facts.running,
            })
            && self.linking.is_none_or(|linking| linking == facts.linking)
            && match self.group {
                GroupFilter::Any => true,
                GroupFilter::Ungrouped => facts.group.is_none(),
                GroupFilter::Group(id) => facts.group.as_ref().is_some_and(|group| group.id == id),
            }
            && matches_search(&SearchQuery::new(&self.query), facts)
    }
}

/// Whether the search finds `facts`: its name, Minecraft version, loader name
/// and version, source, or group name.
#[must_use]
pub fn matches_search(query: &SearchQuery, facts: &InstanceFacts) -> bool {
    let loader = facts.loader.kind.to_string();
    query.matches_any([
        facts.name.as_str(),
        facts.minecraft_version.as_str(),
        loader.as_str(),
        facts.loader.version.as_deref().unwrap_or_default(),
        facts.source.as_str(),
        facts.group.as_ref().map_or("", |group| group.name.as_str()),
    ])
}

/// The key the instance list is ordered by.
///
/// Every key falls back to newest first, then to the id, so repeated refreshes
/// never reorder instances that compare equal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InstanceSort {
    /// Favorites first.
    #[default]
    Favorite,
    /// Name, without regard to case.
    Name,
    /// Newest Minecraft release first.
    Minecraft,
    /// Loader kind, then newest loader version first.
    Loader,
}

impl InstanceSort {
    pub const ALL: [Self; 4] = [Self::Favorite, Self::Name, Self::Minecraft, Self::Loader];

    /// A stable identifier for the configuration file and `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Favorite => "favorite",
            Self::Name => "name",
            Self::Minecraft => "minecraft",
            Self::Loader => "loader",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|sort| sort.as_str() == name)
    }

    /// How `first` and `second` are ordered under this key.
    #[must_use]
    pub fn compare(
        self,
        order: &VersionOrder,
        first: &InstanceFacts,
        second: &InstanceFacts,
    ) -> Ordering {
        let primary = match self {
            Self::Favorite => second.favorite.cmp(&first.favorite),
            Self::Name => natural_compare(&first.name.to_lowercase(), &second.name.to_lowercase()),
            Self::Minecraft => {
                order.compare_newest_first(&first.minecraft_version, &second.minecraft_version)
            }
            Self::Loader => loader_rank(first.loader.kind)
                .cmp(&loader_rank(second.loader.kind))
                .then_with(|| match (&first.loader.version, &second.loader.version) {
                    (Some(a), Some(b)) => natural_compare(b, a),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                }),
        };
        primary
            .then_with(|| second.created_at.cmp(&first.created_at))
            .then_with(|| first.id.cmp(&second.id))
    }

    /// Orders `instances` in place.
    pub fn sort(self, order: &VersionOrder, instances: &mut [InstanceFacts]) {
        instances.sort_by(|first, second| self.compare(order, first, second));
    }
}

const fn loader_rank(kind: LoaderKind) -> u8 {
    match kind {
        LoaderKind::Vanilla => 0,
        LoaderKind::Forge => 1,
        LoaderKind::NeoForge => 2,
    }
}

/// How Minecraft versions are ranked.
///
/// Built from the cached version manifest when one is available, which knows
/// release order exactly. A version the manifest does not list, or any version
/// without a manifest, is ordered by [`natural_compare`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersionOrder {
    ranks: HashMap<String, usize>,
}

impl VersionOrder {
    /// Ranks versions by their position in `newest_first`.
    #[must_use]
    pub fn from_newest_first(newest_first: impl IntoIterator<Item = String>) -> Self {
        let mut ranks = HashMap::new();
        for (rank, id) in newest_first.into_iter().enumerate() {
            ranks.entry(id).or_insert(rank);
        }
        Self { ranks }
    }

    /// Newer versions order first. A listed version orders before an unlisted one.
    #[must_use]
    pub fn compare_newest_first(&self, first: &str, second: &str) -> Ordering {
        match (self.ranks.get(first), self.ranks.get(second)) {
            (Some(a), Some(b)) => a.cmp(b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => natural_compare(second, first),
        }
    }
}

/// Compares version-like text: runs of digits by value, everything else by
/// text, and a text that is a prefix of another first.
#[must_use]
pub fn natural_compare(first: &str, second: &str) -> Ordering {
    let mut first = runs(first);
    let mut second = runs(second);
    loop {
        match (first.next(), second.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) => {
                let ordering = match (a.digits, b.digits) {
                    (true, true) => {
                        let a = a.text.trim_start_matches('0');
                        let b = b.text.trim_start_matches('0');
                        a.len().cmp(&b.len()).then_with(|| a.cmp(b))
                    }
                    _ => a.text.cmp(b.text),
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

struct Run<'a> {
    digits: bool,
    text: &'a str,
}

fn runs(text: &str) -> impl Iterator<Item = Run<'_>> {
    let mut rest = text;
    std::iter::from_fn(move || {
        let first = rest.chars().next()?;
        let digits = first.is_ascii_digit();
        let end = rest
            .find(|c: char| c.is_ascii_digit() != digits)
            .unwrap_or(rest.len());
        let (text, tail) = rest.split_at(end);
        rest = tail;
        Some(Run { digits, text })
    })
}

/// Keeps the instances `filter` matches, ordered by `sort`.
#[must_use]
pub fn list_instances(
    facts: Vec<InstanceFacts>,
    filter: &InstanceFilter,
    sort: InstanceSort,
    order: &VersionOrder,
) -> Vec<InstanceFacts> {
    let mut kept: Vec<InstanceFacts> = facts
        .into_iter()
        .filter(|facts| filter.matches(facts))
        .collect();
    sort.sort(order, &mut kept);
    kept
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn facts(name: &str, minecraft: &str, loader: LoaderSelection) -> InstanceFacts {
        InstanceFacts {
            id: InstanceId::new(),
            name: name.to_owned(),
            storage: StorageClass::Persistent,
            minecraft_version: minecraft.to_owned(),
            loader,
            source: "Manual".to_owned(),
            created_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            favorite: false,
            group: None,
            linking: LinkingState::None,
            running: false,
        }
    }

    fn at(mut facts: InstanceFacts, seconds: i64) -> InstanceFacts {
        facts.created_at = DateTime::from_timestamp(1_700_000_000 + seconds, 0).unwrap();
        facts
    }

    fn names(list: &[InstanceFacts]) -> Vec<&str> {
        list.iter().map(|facts| facts.name.as_str()).collect()
    }

    #[test]
    fn the_default_order_is_favorites_first_then_newest_first() {
        let mut older_favorite = at(facts("old fav", "1.20.1", LoaderSelection::vanilla()), 0);
        older_favorite.favorite = true;
        let newest = at(facts("newest", "1.20.1", LoaderSelection::vanilla()), 30);
        let middle = at(facts("middle", "1.20.1", LoaderSelection::vanilla()), 20);
        let mut sorted = vec![middle, newest, older_favorite];
        InstanceSort::default().sort(&VersionOrder::default(), &mut sorted);
        assert_eq!(names(&sorted), ["old fav", "newest", "middle"]);
    }

    #[test]
    fn a_name_sort_orders_numbers_in_names_by_value() {
        let mut sorted = vec![
            facts("test 10", "1.20.1", LoaderSelection::vanilla()),
            facts("Test 2", "1.20.1", LoaderSelection::vanilla()),
            facts("test 1", "1.20.1", LoaderSelection::vanilla()),
        ];
        InstanceSort::Name.sort(&VersionOrder::default(), &mut sorted);
        assert_eq!(names(&sorted), ["test 1", "Test 2", "test 10"]);
    }

    #[test]
    fn a_name_sort_ignores_case_and_falls_back_to_newest_then_id() {
        let a = at(facts("alpha", "1.20.1", LoaderSelection::vanilla()), 1);
        let b = at(facts("Beta", "1.20.1", LoaderSelection::vanilla()), 2);
        let tie_old = at(facts("gamma", "1.20.1", LoaderSelection::vanilla()), 3);
        let tie_new = at(facts("GAMMA", "1.20.1", LoaderSelection::vanilla()), 4);
        let mut sorted = vec![tie_old, b, tie_new, a];
        InstanceSort::Name.sort(&VersionOrder::default(), &mut sorted);
        assert_eq!(names(&sorted), ["alpha", "Beta", "GAMMA", "gamma"]);
    }

    #[test]
    fn equal_keys_end_in_id_order_whatever_the_input_order() {
        let first = facts("same", "1.20.1", LoaderSelection::vanilla());
        let mut second = facts("same", "1.20.1", LoaderSelection::vanilla());
        second.created_at = first.created_at;
        let expected = {
            let mut pair = vec![first.clone(), second.clone()];
            pair.sort_by_key(|facts| facts.id);
            pair
        };
        for sort in InstanceSort::ALL {
            let mut forward = vec![first.clone(), second.clone()];
            let mut backward = vec![second.clone(), first.clone()];
            sort.sort(&VersionOrder::default(), &mut forward);
            sort.sort(&VersionOrder::default(), &mut backward);
            assert_eq!(forward, expected, "{sort:?}");
            assert_eq!(backward, expected, "{sort:?}");
        }
    }

    #[test]
    fn minecraft_versions_order_by_the_manifest_when_it_lists_them() {
        let order =
            VersionOrder::from_newest_first(["1.21", "1.20.6", "1.20.1", "1.9"].map(str::to_owned));
        let mut sorted = vec![
            facts("a", "1.9", LoaderSelection::vanilla()),
            facts("b", "1.20.1", LoaderSelection::vanilla()),
            facts("c", "1.21", LoaderSelection::vanilla()),
        ];
        InstanceSort::Minecraft.sort(&order, &mut sorted);
        assert_eq!(names(&sorted), ["c", "b", "a"]);
    }

    #[test]
    fn minecraft_versions_without_a_manifest_order_by_number_not_by_text() {
        let mut sorted = vec![
            facts("a", "1.9", LoaderSelection::vanilla()),
            facts("b", "1.20.1", LoaderSelection::vanilla()),
            facts("c", "1.20", LoaderSelection::vanilla()),
        ];
        InstanceSort::Minecraft.sort(&VersionOrder::default(), &mut sorted);
        assert_eq!(names(&sorted), ["b", "c", "a"]);
    }

    #[test]
    fn loaders_order_by_kind_then_newest_version() {
        let mut sorted = vec![
            facts(
                "neo",
                "1.21",
                LoaderSelection::exact(LoaderKind::NeoForge, "21.0.1"),
            ),
            facts(
                "forge old",
                "1.20.1",
                LoaderSelection::exact(LoaderKind::Forge, "47.2.0"),
            ),
            facts("vanilla", "1.20.1", LoaderSelection::vanilla()),
            facts(
                "forge new",
                "1.20.1",
                LoaderSelection::exact(LoaderKind::Forge, "47.10.0"),
            ),
        ];
        InstanceSort::Loader.sort(&VersionOrder::default(), &mut sorted);
        assert_eq!(names(&sorted), ["vanilla", "forge new", "forge old", "neo"]);
    }

    #[test]
    fn filters_combine_so_every_set_condition_must_hold() {
        let mut matching = facts(
            "repro",
            "1.20.1",
            LoaderSelection::exact(LoaderKind::Forge, "47.2.0"),
        );
        matching.favorite = true;
        matching.running = true;
        let wrong_loader = {
            let mut other = matching.clone();
            other.id = InstanceId::new();
            other.loader = LoaderSelection::vanilla();
            other
        };
        let not_favorite = {
            let mut other = matching.clone();
            other.id = InstanceId::new();
            other.favorite = false;
            other
        };
        let filter = InstanceFilter {
            favorites_only: true,
            loader: Some(LoaderKind::Forge),
            run_state: Some(RunFilter::Running),
            ..InstanceFilter::default()
        };
        let kept = list_instances(
            vec![wrong_loader, matching.clone(), not_favorite],
            &filter,
            InstanceSort::default(),
            &VersionOrder::default(),
        );
        assert_eq!(kept, [matching]);
        assert_eq!(filter.active_count(), 3);
    }

    #[test]
    fn a_default_filter_is_inactive_and_keeps_everything() {
        let filter = InstanceFilter::default();
        assert!(!filter.is_active());
        assert!(filter.matches(&facts("x", "1.20.1", LoaderSelection::vanilla())));
        assert!(
            !InstanceFilter {
                query: "  ".to_owned(),
                ..InstanceFilter::default()
            }
            .is_active()
        );
    }

    #[test]
    fn the_group_filter_separates_a_group_from_ungrouped_instances() {
        let group = InstanceGroup {
            id: GroupId::new(),
            name: "Regression".to_owned(),
            tone: 0,
        };
        let mut grouped = facts("in", "1.20.1", LoaderSelection::vanilla());
        grouped.group = Some(group.clone());
        let loose = facts("out", "1.20.1", LoaderSelection::vanilla());
        let by_group = InstanceFilter {
            group: GroupFilter::Group(group.id),
            ..InstanceFilter::default()
        };
        let ungrouped = InstanceFilter {
            group: GroupFilter::Ungrouped,
            ..InstanceFilter::default()
        };
        assert!(by_group.matches(&grouped) && !by_group.matches(&loose));
        assert!(!ungrouped.matches(&grouped) && ungrouped.matches(&loose));
    }

    #[test]
    fn search_is_unicode_case_insensitive_and_covers_metadata_and_group() {
        let mut instance = facts(
            "Ärger Test",
            "1.20.1",
            LoaderSelection::exact(LoaderKind::NeoForge, "21.0.1"),
        );
        instance.source = "Modrinth some-pack".to_owned();
        instance.group = Some(InstanceGroup {
            id: GroupId::new(),
            name: "Überprüfung".to_owned(),
            tone: 0,
        });
        for query in [
            "ärger",
            "ÄRGER",
            "1.20",
            "neoforge",
            "21.0",
            "some-pack",
            "überprüf",
        ] {
            assert!(
                matches_search(&SearchQuery::new(query), &instance),
                "{query}"
            );
        }
        assert!(!matches_search(&SearchQuery::new("missing"), &instance));
        assert!(matches_search(&SearchQuery::new("   "), &instance));
    }

    #[test]
    fn an_unknown_sort_name_is_not_a_sort() {
        assert_eq!(InstanceSort::from_name("name"), Some(InstanceSort::Name));
        assert_eq!(InstanceSort::from_name("popularity"), None);
    }

    #[test]
    fn natural_comparison_reads_digit_runs_as_numbers() {
        assert_eq!(natural_compare("1.9", "1.10"), Ordering::Less);
        assert_eq!(natural_compare("1.20", "1.20.1"), Ordering::Less);
        assert_eq!(natural_compare("47.02.0", "47.2.0"), Ordering::Equal);
    }
}
