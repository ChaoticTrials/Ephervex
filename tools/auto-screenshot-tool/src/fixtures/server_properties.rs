//! A server's Properties rows, built from the launcher's property catalog the
//! way its form builds them for a file the server has not started with yet:
//! every key that is not advanced, with the stored value or what an absent
//! key falls back to, and no unsaved edits.

use ephervex_core::{
    PropertyDefinition, PropertyGroup, PropertyValueKind, SERVER_PROPERTY_CATALOG,
};
use ephervex_gui::ui::{ServerPropertyGroup, ServerPropertyRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::{ModelRc, SharedString};

use super::rows;

/// The rows for a file holding `stored`, split into the cards that show them.
/// A stored choice is written as one of its names.
pub(crate) fn property_groups(stored: &[(&str, &str)]) -> ModelRc<ServerPropertyGroup> {
    let mut groups: Vec<(PropertyGroup, Vec<ServerPropertyRow>)> = Vec::new();
    for definition in SERVER_PROPERTY_CATALOG {
        if definition.group == PropertyGroup::Advanced {
            continue;
        }
        let value = stored
            .iter()
            .find(|(key, _)| *key == definition.key)
            .map(|(_, value)| *value);
        let row = property_row(definition, value);
        match groups.last_mut() {
            Some((group, rows)) if *group == definition.group => rows.push(row),
            _ => groups.push((definition.group, vec![row])),
        }
    }
    rows(
        groups
            .into_iter()
            .map(|(group, group_rows)| ServerPropertyGroup {
                heading: group_heading(group).into(),
                rows: rows(group_rows),
            })
            .collect(),
    )
}

fn group_heading(group: PropertyGroup) -> String {
    lookup(match group {
        PropertyGroup::World => "gui-property-group-world",
        PropertyGroup::Gameplay => "gui-property-group-gameplay",
        PropertyGroup::Players => "gui-property-group-players",
        PropertyGroup::Performance => "gui-property-group-performance",
        PropertyGroup::Advanced => "gui-property-group-advanced",
    })
}

fn property_row(definition: &PropertyDefinition, value: Option<&str>) -> ServerPropertyRow {
    let fallback = definition.vanilla_default;
    let note = match value {
        Some(_) => String::new(),
        None if fallback.is_empty() => lookup("gui-property-not-set-empty"),
        None => lookup_args("gui-property-not-set", [("value", Arg::from(fallback))]),
    };
    let mut row = ServerPropertyRow {
        key: definition.key.into(),
        label: lookup(&format!("gui-property-{}", definition.key)).into(),
        detail: lookup(&format!("gui-property-{}-detail", definition.key)).into(),
        kind: "text".into(),
        text: value.unwrap_or_default().into(),
        placeholder: fallback.into(),
        note: note.into(),
        ..ServerPropertyRow::default()
    };
    match definition.kind {
        PropertyValueKind::Toggle => {
            row.kind = "toggle".into();
            row.checked = value.unwrap_or(fallback).eq_ignore_ascii_case("true");
        }
        PropertyValueKind::Integer { .. } => row.kind = "integer".into(),
        PropertyValueKind::Choice(names) => {
            row.kind = "choice".into();
            // A key the file does not hold offers to stay unset, first.
            let unset = value.is_none().then(|| {
                lookup_args(
                    "gui-property-unset-choice",
                    [("value", Arg::from(fallback))],
                )
            });
            let choices: Vec<SharedString> = unset
                .into_iter()
                .chain(names.iter().map(|name| (*name).to_owned()))
                .map(SharedString::from)
                .collect();
            let position = value.and_then(|value| names.iter().position(|name| *name == value));
            row.choice_index = position.map_or(0, |position| i32::try_from(position).unwrap_or(0));
            row.choices = rows(choices);
        }
        PropertyValueKind::Text => {}
    }
    row
}

#[cfg(test)]
mod tests {
    use slint::Model;

    use super::*;

    #[test]
    fn a_stored_choice_is_selected_and_an_absent_one_offers_to_stay_unset() {
        let groups = property_groups(&[("difficulty", "normal")]);
        let rows: Vec<ServerPropertyRow> = groups
            .iter()
            .flat_map(|group| group.rows.iter().collect::<Vec<_>>())
            .collect();
        let difficulty = rows.iter().find(|row| row.key == "difficulty");
        let gamemode = rows.iter().find(|row| row.key == "gamemode");
        assert!(difficulty.is_some_and(|row| {
            row.choices
                .row_data(usize::try_from(row.choice_index).unwrap_or(0))
                == Some("normal".into())
        }));
        assert!(gamemode.is_some_and(|row| row.choice_index == 0 && !row.note.is_empty()));
    }

    #[test]
    fn advanced_keys_are_left_out() {
        let groups = property_groups(&[]);
        let advanced = group_heading(PropertyGroup::Advanced);
        assert!(
            groups
                .iter()
                .all(|group| group.heading != advanced.as_str())
        );
        assert!(groups.row_count() > 1);
    }
}
