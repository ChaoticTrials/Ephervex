//! Favorites, groups, and icons on the Instances page. Which instances are
//! favorites, how they are grouped, and which recorded mod logos serve as
//! icons are choices made for the capture.

use ephervex_gui::ui::{AppWindow, GroupRow, HostIcons, InstanceRow};
use ephervex_i18n::lookup;
use slint::{ComponentHandle, Image, SharedString};

use super::{instance_rows, recorded, rows};
use crate::artwork;

struct Group {
    id: &'static str,
    name: &'static str,
    /// The color a group gets when it is created: the least used one, so the
    /// first group created takes the first color.
    tone: i32,
    members: &'static [&'static str],
}

/// The groups in the order the launcher lists them, by name ignoring case.
const GROUPS: [Group; 3] = [
    Group {
        id: "bug-reports",
        name: "Bug reports",
        tone: 2,
        members: &[],
    },
    Group {
        id: "mod-development",
        name: "Mod development",
        tone: 1,
        members: &["utility-mods", "skyblock-tools"],
    },
    Group {
        id: "modpacks",
        name: "Modpacks",
        tone: 0,
        members: &["cavestone", "photon", "cobblemon", "fog", "super-tiny"],
    },
];

const FAVORITES: [&str; 2] = ["cavestone", "utility-mods"];

/// Instances shown with the logo of a recorded mod they are built to test.
const ICONS: [(&str, &str); 2] = [
    ("utility-mods", "UtilitiX"),
    ("skyblock-tools", "Skyblock Builder"),
];

/// The recorded instances with favorites, groups, and icons, listed the way
/// the default sort order lists them: favorites first.
fn organized_rows(ui: &AppWindow) -> Vec<InstanceRow> {
    let mut images: Vec<Image> = Vec::new();
    let mut slots = Vec::new();
    for (key, title) in ICONS {
        let image = recorded::data()
            .cavestone_mods
            .iter()
            .find(|row| row.title == title)
            .map(|row| artwork::logo(&row.icon.url))
            .unwrap_or_default();
        // An unavailable logo is shown the way an unreadable icon file is:
        // as the loader symbol.
        if image.size().width > 0 {
            slots.push((key, i32::try_from(images.len()).unwrap_or(-1)));
            images.push(image);
        }
    }
    ui.global::<HostIcons>().set_images(rows(images));

    let mut instances: Vec<InstanceRow> = instance_rows()
        .into_iter()
        .map(|row| {
            let key = row.id.as_str();
            let group = GROUPS.iter().position(|group| group.members.contains(&key));
            InstanceRow {
                favorite: FAVORITES.contains(&key),
                group_id: group.map_or("", |index| GROUPS[index].id).into(),
                group_name: group.map_or("", |index| GROUPS[index].name).into(),
                group_index: group.map_or(-1, |index| i32::try_from(index).unwrap_or(-1)),
                group_tone: group.map_or(0, |index| GROUPS[index].tone),
                icon_slot: slots
                    .iter()
                    .find(|(slot_key, _)| *slot_key == key)
                    .map_or(-1, |(_, slot)| *slot),
                ..row
            }
        })
        .collect();
    instances.sort_by_key(|row| !row.favorite);

    ui.set_instance_groups(rows(
        GROUPS
            .iter()
            .map(|group| GroupRow {
                id: group.id.into(),
                name: group.name.into(),
                tone: group.tone,
                members: i32::try_from(group.members.len()).unwrap_or(i32::MAX),
            })
            .collect(),
    ));
    ui.set_instance_group_names(rows(
        GROUPS
            .iter()
            .map(|group| SharedString::from(group.name))
            .collect(),
    ));
    instances
}

fn show(ui: &AppWindow, instances: Vec<InstanceRow>, selected: &str) {
    let index = instances
        .iter()
        .position(|row| row.id == selected)
        .map_or(-1, |index| i32::try_from(index).unwrap_or(-1));
    ui.set_instances(rows(instances));
    ui.set_selected_instance_id(selected.into());
    ui.set_selected_instance_index(index);
    ui.set_workspace_open(false);
    ui.set_page(0);
}

/// Favorites first, group labels, and icons, with a grouped instance that has
/// an icon selected.
pub(crate) fn instance_organization(ui: &AppWindow) {
    let instances = organized_rows(ui);
    show(ui, instances, "utility-mods");
}

/// The filter panel narrowing the list to one loader.
pub(crate) fn instances_filters(ui: &AppWindow) {
    let instances: Vec<InstanceRow> = organized_rows(ui)
        .into_iter()
        .filter(|row| row.loader_kind == "neoforge")
        .collect();
    let labels =
        |ids: &[&str]| -> Vec<SharedString> { ids.iter().map(|id| lookup(id).into()).collect() };
    let mut minecraft = labels(&["gui-filter-any-version"]);
    minecraft.extend(["1.21.1", "1.20.1", "1.12.2"].map(SharedString::from));
    ui.set_instance_minecraft_options(rows(minecraft));
    ui.set_instance_loader_options(rows(labels(&[
        "gui-filter-any-loader",
        "gui-loader-vanilla",
        "gui-loader-forge",
        "gui-loader-neoforge",
    ])));
    ui.set_instance_run_options(rows(labels(&[
        "gui-filter-any-run-state",
        "gui-filter-running",
        "gui-filter-stopped",
    ])));
    ui.set_instance_linking_options(rows(labels(&[
        "gui-filter-any-linking",
        "gui-filter-linking-paired",
        "gui-filter-linking-servers",
        "gui-filter-linking-none",
    ])));
    let mut groups = labels(&["gui-filter-any-group", "gui-filter-ungrouped"]);
    groups.extend(GROUPS.iter().map(|group| SharedString::from(group.name)));
    ui.set_instance_group_options(rows(groups));
    ui.set_instance_sort_options(rows(labels(&[
        "gui-sort-favorites",
        "gui-sort-name",
        "gui-sort-minecraft",
        "gui-sort-loader",
    ])));
    ui.set_instance_loader_index(3);
    ui.set_instance_filters_active(1);
    ui.set_instance_filters_open(true);
    show(ui, instances, "utility-mods");
}

/// The groups dialog, opened from the Instances toolbar.
pub(crate) fn instance_groups_dialog(ui: &AppWindow) {
    instance_organization(ui);
    ui.set_modal("groups".into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_grouped_instance_and_icon_names_a_recorded_instance() {
        let keys: Vec<String> = recorded::data()
            .instances
            .iter()
            .map(|record| record.key.clone())
            .collect();
        let named = GROUPS
            .iter()
            .flat_map(|group| group.members.iter())
            .chain(FAVORITES.iter())
            .chain(ICONS.iter().map(|(key, _)| key));
        for key in named {
            assert!(keys.iter().any(|recorded| recorded == key), "{key}");
        }
        for (_, title) in ICONS {
            assert!(
                recorded::data()
                    .cavestone_mods
                    .iter()
                    .any(|row| row.title == title),
                "{title}"
            );
        }
    }

    #[test]
    fn groups_are_listed_by_name() {
        let names: Vec<String> = GROUPS
            .iter()
            .map(|group| group.name.to_lowercase())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }
}
