//! A server's Properties form: one row per catalog key, built from what the
//! server's file holds, the user's unsaved edits, and when the server reads
//! them.

use std::collections::{BTreeMap, BTreeSet};

use ephervex_core::{
    PropertyDefinition, PropertyGroup, PropertyValueKind, PropertyValueProblem,
    SERVER_PROPERTY_CATALOG,
};
use ephervex_i18n::{Arg, lookup, lookup_args};

/// The largest bound a range message names; above it only the minimum is.
const NAMED_MAXIMUM: i64 = 1_000_000_000;

/// What the form starts from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StoredProperties {
    /// The catalog keys the server's next start reads, with their values.
    pub(crate) values: BTreeMap<String, String>,
    /// Keys saved while the running process was already up.
    pub(crate) awaiting_restart: BTreeSet<String>,
    /// Whether the file lists every key the server's release reads, which a
    /// server writes once a start gets far enough. A key the file then lacks
    /// is one that release does not read.
    pub(crate) lists_every_key: bool,
}

/// Keys every release's server writes into its file and that neither this
/// form nor Ephervex's typed settings ever write.
const WRITTEN_ONLY_BY_THE_SERVER: &[&str] = &["enable-rcon", "enable-query"];

/// Whether a server's file lists every key its release reads. A server writes
/// them once a start gets far enough; a start that failed sooner leaves only
/// what Ephervex and this form wrote, which never includes
/// [`WRITTEN_ONLY_BY_THE_SERVER`].
pub(crate) fn file_lists_every_key(has_started: bool, holds: impl Fn(&str) -> bool) -> bool {
    has_started && WRITTEN_ONLY_BY_THE_SERVER.iter().all(|key| holds(key))
}

/// The user's edits that are not saved yet, by key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PropertyDrafts {
    edits: BTreeMap<String, String>,
}

impl PropertyDrafts {
    /// Records `value` for `key`; an edit back to what is stored is no edit,
    /// and neither is an empty field for a key the file does not hold.
    pub(crate) fn edit(&mut self, stored: &StoredProperties, key: &str, value: &str) {
        let unchanged = match stored.values.get(key) {
            Some(current) => current == value,
            None => value.is_empty(),
        };
        if unchanged {
            self.edits.remove(key);
        } else {
            self.edits.insert(key.to_owned(), value.to_owned());
        }
    }

    /// Picks entry `index` of the key's choice list.
    pub(crate) fn choose(&mut self, stored: &StoredProperties, key: &str, index: usize) {
        let Some(definition) = ephervex_core::property_definition(key) else {
            return;
        };
        let current = stored.values.get(key).map(String::as_str);
        let entries = choice_entries(definition, current);
        let Some(entry) = entries.get(index) else {
            return;
        };
        let value = match entry {
            ChoiceEntry::Unset => String::new(),
            ChoiceEntry::Stored(value) => value.clone(),
            ChoiceEntry::Name(position) => {
                let names = choice_names(definition);
                // A number is what every release reads: servers before
                // Minecraft 1.14 replace a name with their own default. A
                // stored name stays a name.
                if current.is_none_or(is_number) {
                    position.to_string()
                } else {
                    names[*position].to_owned()
                }
            }
        };
        self.edit(stored, key, &value);
    }

    pub(crate) fn clear(&mut self) {
        self.edits.clear();
    }

    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.edits.len()
    }

    /// The edits to save, or `None` while one of them is a value its key
    /// does not accept.
    #[must_use]
    pub(crate) fn values(&self) -> Option<Vec<(String, String)>> {
        self.edits
            .iter()
            .map(|(key, value)| {
                ephervex_core::check_property_value(key, value)
                    .ok()
                    .map(|()| (key.clone(), value.clone()))
            })
            .collect()
    }
}

/// The Properties form of each server a pane has shown: what it started from,
/// the unsaved edits, and whether advanced keys are listed.
#[derive(Debug, Default)]
pub(crate) struct ServerPropertyForms {
    forms: std::collections::HashMap<String, PropertyForm>,
}

#[derive(Debug, Default)]
struct PropertyForm {
    stored: StoredProperties,
    drafts: PropertyDrafts,
    show_advanced: bool,
}

/// What a form shows, ready for a host view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PropertyFormView {
    pub(crate) rows: Vec<PropertyRow>,
    pub(crate) edits: usize,
    pub(crate) edits_valid: bool,
    pub(crate) show_advanced: bool,
}

impl ServerPropertyForms {
    /// Starts `server`'s form over from what was just read. Unsaved edits go.
    pub(crate) fn load(&mut self, server: &str, stored: StoredProperties) {
        let form = self.forms.entry(server.to_owned()).or_default();
        form.stored = stored;
        form.drafts.clear();
    }

    pub(crate) fn edit(&mut self, server: &str, key: &str, value: &str) -> bool {
        self.forms.get_mut(server).is_some_and(|form| {
            form.drafts.edit(&form.stored, key, value);
            true
        })
    }

    pub(crate) fn choose(&mut self, server: &str, key: &str, index: usize) -> bool {
        self.forms.get_mut(server).is_some_and(|form| {
            form.drafts.choose(&form.stored, key, index);
            true
        })
    }

    pub(crate) fn show_advanced(&mut self, server: &str, show: bool) -> bool {
        self.forms.get_mut(server).is_some_and(|form| {
            form.show_advanced = show;
            true
        })
    }

    pub(crate) fn discard(&mut self, server: &str) -> bool {
        self.forms.get_mut(server).is_some_and(|form| {
            form.drafts.clear();
            true
        })
    }

    /// The edits to save, none without a form, or `None` while one of them
    /// is invalid.
    #[must_use]
    pub(crate) fn values(&self, server: &str) -> Option<Vec<(String, String)>> {
        self.forms
            .get(server)
            .map_or_else(|| Some(Vec::new()), |form| form.drafts.values())
    }

    #[must_use]
    pub(crate) fn view(&self, server: &str) -> Option<PropertyFormView> {
        self.forms.get(server).map(|form| PropertyFormView {
            rows: property_rows(&form.stored, &form.drafts, form.show_advanced),
            edits: form.drafts.len(),
            edits_valid: form.drafts.values().is_some(),
            show_advanced: form.show_advanced,
        })
    }
}

/// The Settings form for the `server.properties` values new servers start
/// with: what is stored, the unsaved edits, and whether advanced keys are
/// listed. Unlike a server's file, a default can be taken away again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ServerDefaultsForm {
    stored: BTreeMap<String, String>,
    /// `None` takes the stored default away.
    edits: BTreeMap<String, Option<String>>,
    show_advanced: bool,
}

impl ServerDefaultsForm {
    /// Takes the stored defaults. Unsaved edits stay until what is stored
    /// changes, which a save does. Returns whether it changed.
    pub(crate) fn load(&mut self, stored: BTreeMap<String, String>) -> bool {
        if self.stored == stored {
            return false;
        }
        self.stored = stored;
        self.edits.clear();
        true
    }

    /// Sets `key`'s default; empty text takes it away.
    pub(crate) fn edit(&mut self, key: &str, value: &str) {
        let wanted = (!value.is_empty()).then(|| value.to_owned());
        if self.stored.get(key) == wanted.as_ref() {
            self.edits.remove(key);
        } else {
            self.edits.insert(key.to_owned(), wanted);
        }
    }

    /// Picks entry `index` of a choice list, whose first entry is no default.
    pub(crate) fn choose(&mut self, key: &str, index: usize) {
        let Some(definition) = ephervex_core::property_definition(key) else {
            return;
        };
        let names = choice_names(definition);
        match index.checked_sub(1) {
            None => self.edit(key, ""),
            Some(position) if position < names.len() => self.edit(key, names[position]),
            Some(_) => {}
        }
    }

    pub(crate) fn reset(&mut self, key: &str) {
        self.edit(key, "");
    }

    pub(crate) fn discard(&mut self) {
        self.edits.clear();
    }

    pub(crate) fn show_advanced(&mut self, show: bool) {
        self.show_advanced = show;
    }

    /// The defaults to store, or `None` while an edit is a value its key does
    /// not accept.
    #[must_use]
    pub(crate) fn values(&self) -> Option<BTreeMap<String, String>> {
        let mut values = self.stored.clone();
        for (key, edit) in &self.edits {
            match edit {
                Some(value) => {
                    ephervex_core::check_property_value(key, value).ok()?;
                    values.insert(key.clone(), value.clone());
                }
                None => {
                    values.remove(key);
                }
            }
        }
        Some(values)
    }

    #[must_use]
    pub(crate) fn view(&self) -> PropertyFormView {
        let mut rows = Vec::new();
        let mut last_group = None;
        for definition in SERVER_PROPERTY_CATALOG {
            if definition.group == PropertyGroup::Advanced && !self.show_advanced {
                continue;
            }
            let heading = if last_group == Some(definition.group) {
                String::new()
            } else {
                group_heading(definition.group)
            };
            last_group = Some(definition.group);
            rows.push(self.row(definition, heading));
        }
        PropertyFormView {
            rows,
            edits: self.edits.len(),
            edits_valid: self.values().is_some(),
            show_advanced: self.show_advanced,
        }
    }

    fn row(&self, definition: &'static PropertyDefinition, heading: String) -> PropertyRow {
        let edit = self.edits.get(definition.key);
        let shown = match edit {
            Some(edit) => edit.as_deref(),
            None => self.stored.get(definition.key).map(String::as_str),
        };
        let vanilla = definition.vanilla_default;
        let error = edit
            .and_then(Option::as_deref)
            .and_then(|value| definition.kind.check(value).err())
            .map_or_else(String::new, problem_message);
        let note = match shown {
            Some(_) => String::new(),
            None if vanilla.is_empty() => lookup("gui-property-default-unset-empty"),
            None => lookup_args(
                "gui-property-default-unset",
                [("value", Arg::from(vanilla))],
            ),
        };
        let mut row = PropertyRow {
            key: definition.key,
            heading,
            label: lookup(&format!("gui-property-{}", definition.key)),
            detail: lookup(&format!("gui-property-{}-detail", definition.key)),
            kind: "text",
            text: shown.unwrap_or_default().to_owned(),
            placeholder: vanilla.to_owned(),
            checked: false,
            choices: Vec::new(),
            choice_index: 0,
            note,
            changed: edit.is_some(),
            awaiting_restart: false,
            error,
            can_reset: shown.is_some(),
        };
        match definition.kind {
            PropertyValueKind::Toggle => {
                row.kind = "toggle";
                row.checked = shown.unwrap_or(vanilla).eq_ignore_ascii_case("true");
            }
            PropertyValueKind::Integer { .. } => row.kind = "integer",
            PropertyValueKind::Choice(names) => {
                row.kind = "choice";
                row.choices = std::iter::once(lookup_args(
                    "gui-property-unset-choice",
                    [("value", Arg::from(vanilla))],
                ))
                .chain(names.iter().map(|name| (*name).to_owned()))
                .collect();
                row.choice_index = shown
                    .and_then(|value| name_position(definition, value))
                    .and_then(|position| i32::try_from(position + 1).ok())
                    .unwrap_or(0);
            }
            PropertyValueKind::Text => {}
        }
        row
    }
}

/// One entry of a choice list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChoiceEntry {
    /// Leaves a key the file does not hold out of it.
    Unset,
    /// A stored value no name stands for, kept while it stays selected.
    Stored(String),
    Name(usize),
}

fn choice_names(definition: &PropertyDefinition) -> &'static [&'static str] {
    match definition.kind {
        PropertyValueKind::Choice(names) => names,
        _ => &[],
    }
}

fn is_number(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn name_position(definition: &PropertyDefinition, value: &str) -> Option<usize> {
    let names = choice_names(definition);
    names.iter().position(|name| *name == value).or_else(|| {
        value
            .parse::<usize>()
            .ok()
            .filter(|position| is_number(value) && *position < names.len())
    })
}

fn choice_entries(definition: &PropertyDefinition, stored: Option<&str>) -> Vec<ChoiceEntry> {
    let names = (0..choice_names(definition).len()).map(ChoiceEntry::Name);
    match stored {
        None => std::iter::once(ChoiceEntry::Unset).chain(names).collect(),
        Some(value) if name_position(definition, value).is_none() => {
            std::iter::once(ChoiceEntry::Stored(value.to_owned()))
                .chain(names)
                .collect()
        }
        Some(_) => names.collect(),
    }
}

/// One row as the form shows it. Its flags mirror the row the markup draws,
/// which is a plain record rather than a state to encode.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PropertyRow {
    pub(crate) key: &'static str,
    /// Non-empty on the first row of a group.
    pub(crate) heading: String,
    pub(crate) label: String,
    pub(crate) detail: String,
    /// `toggle`, `integer`, `choice` or `text`.
    pub(crate) kind: &'static str,
    pub(crate) text: String,
    pub(crate) placeholder: String,
    pub(crate) checked: bool,
    pub(crate) choices: Vec<String>,
    pub(crate) choice_index: i32,
    /// Muted text after the description, such as the value a key the file
    /// does not hold falls back to.
    pub(crate) note: String,
    pub(crate) changed: bool,
    pub(crate) awaiting_restart: bool,
    pub(crate) error: String,
    /// Whether the row offers to go back to having no value of its own.
    pub(crate) can_reset: bool,
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

fn problem_message(problem: PropertyValueProblem) -> String {
    match problem {
        PropertyValueProblem::NotToggle => lookup("gui-property-toggle-invalid"),
        PropertyValueProblem::NotWholeNumber => lookup("gui-property-whole-number"),
        PropertyValueProblem::OutOfRange { minimum, maximum } if maximum >= NAMED_MAXIMUM => {
            lookup_args("gui-property-minimum", [("minimum", Arg::from(minimum))])
        }
        PropertyValueProblem::OutOfRange { minimum, maximum } => lookup_args(
            "gui-property-range",
            [
                ("minimum", Arg::from(minimum)),
                ("maximum", Arg::from(maximum)),
            ],
        ),
        PropertyValueProblem::UnknownChoice => lookup("gui-property-choice-invalid"),
    }
}

/// The rows to show. Advanced keys are listed only when `show_advanced`, and
/// a server that has run lists only the keys its file holds.
#[must_use]
pub(crate) fn property_rows(
    stored: &StoredProperties,
    drafts: &PropertyDrafts,
    show_advanced: bool,
) -> Vec<PropertyRow> {
    let mut rows = Vec::new();
    let mut last_group = None;
    for definition in SERVER_PROPERTY_CATALOG {
        if definition.group == PropertyGroup::Advanced && !show_advanced {
            continue;
        }
        let current = stored.values.get(definition.key).map(String::as_str);
        let draft = drafts.edits.get(definition.key).map(String::as_str);
        if stored.lists_every_key && current.is_none() && draft.is_none() {
            continue;
        }
        let heading = if last_group == Some(definition.group) {
            String::new()
        } else {
            group_heading(definition.group)
        };
        last_group = Some(definition.group);
        rows.push(property_row(stored, definition, current, draft, heading));
    }
    rows
}

fn property_row(
    stored: &StoredProperties,
    definition: &'static PropertyDefinition,
    current: Option<&str>,
    draft: Option<&str>,
    heading: String,
) -> PropertyRow {
    let shown = draft.or(current);
    let fallback = definition.vanilla_default;
    let error = draft
        .and_then(|value| definition.kind.check(value).err())
        .map_or_else(String::new, problem_message);
    let note = match shown {
        Some(_) => String::new(),
        None if fallback.is_empty() => lookup("gui-property-not-set-empty"),
        None => lookup_args("gui-property-not-set", [("value", Arg::from(fallback))]),
    };
    let mut row = PropertyRow {
        key: definition.key,
        heading,
        label: lookup(&format!("gui-property-{}", definition.key)),
        detail: lookup(&format!("gui-property-{}-detail", definition.key)),
        kind: "text",
        text: shown.unwrap_or_default().to_owned(),
        placeholder: fallback.to_owned(),
        checked: false,
        choices: Vec::new(),
        choice_index: 0,
        note,
        changed: draft.is_some(),
        awaiting_restart: stored.awaiting_restart.contains(definition.key),
        error,
        can_reset: false,
    };
    match definition.kind {
        PropertyValueKind::Toggle => {
            row.kind = "toggle";
            row.checked = shown.unwrap_or(fallback).eq_ignore_ascii_case("true");
        }
        PropertyValueKind::Integer { .. } => row.kind = "integer",
        PropertyValueKind::Choice(names) => {
            row.kind = "choice";
            let entries = choice_entries(definition, current);
            row.choices = entries
                .iter()
                .map(|entry| match entry {
                    ChoiceEntry::Unset => lookup_args(
                        "gui-property-unset-choice",
                        [("value", Arg::from(fallback))],
                    ),
                    ChoiceEntry::Stored(value) => value.clone(),
                    ChoiceEntry::Name(position) => names[*position].to_owned(),
                })
                .collect();
            let selected = match shown {
                None => Some(ChoiceEntry::Unset),
                Some(value) => match name_position(definition, value) {
                    Some(position) => Some(ChoiceEntry::Name(position)),
                    None => Some(ChoiceEntry::Stored(value.to_owned())),
                },
            };
            row.choice_index = selected
                .and_then(|selected| entries.iter().position(|entry| *entry == selected))
                .and_then(|index| i32::try_from(index).ok())
                .unwrap_or(0);
        }
        PropertyValueKind::Text => {}
    }
    row
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn stored(values: &[(&str, &str)]) -> StoredProperties {
        StoredProperties {
            values: values
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            ..StoredProperties::default()
        }
    }

    fn row<'a>(rows: &'a [PropertyRow], key: &str) -> &'a PropertyRow {
        rows.iter().find(|row| row.key == key).expect(key)
    }

    #[test]
    fn a_server_that_has_run_lists_only_the_keys_its_file_holds() {
        let mut properties = stored(&[("motd", "hello"), ("pvp", "true")]);
        let all = property_rows(&properties, &PropertyDrafts::default(), false);
        assert!(all.iter().any(|row| row.key == "spawn-animals"));

        properties.lists_every_key = true;
        let rows = property_rows(&properties, &PropertyDrafts::default(), false);
        let keys: Vec<_> = rows.iter().map(|row| row.key).collect();
        assert_eq!(keys, ["pvp", "motd"]);
    }

    #[test]
    fn a_start_that_failed_before_the_server_wrote_its_keys_keeps_every_row() {
        // What a save made before the first start leaves behind.
        let saved_by_the_form = ["server-port", "level-name", "motd", "difficulty"];
        let holds = |file: &'static [&'static str]| move |key: &str| file.contains(&key);
        assert!(!file_lists_every_key(true, holds(&[])));
        assert!(!file_lists_every_key(true, |key| saved_by_the_form.contains(&key)));
        let written = &["level-name", "enable-rcon", "enable-query"];
        assert!(file_lists_every_key(true, holds(written)));
        assert!(!file_lists_every_key(false, holds(written)));

        let properties = StoredProperties {
            lists_every_key: file_lists_every_key(true, |key| saved_by_the_form.contains(&key)),
            ..StoredProperties::default()
        };
        let rows = property_rows(&properties, &PropertyDrafts::default(), false);
        assert!(rows.iter().any(|row| row.key == "motd"));
    }

    #[test]
    fn advanced_keys_are_listed_only_when_asked_for() {
        let properties = stored(&[]);
        let basic = property_rows(&properties, &PropertyDrafts::default(), false);
        assert!(basic.iter().all(|row| row.key != "rate-limit"));
        let advanced = property_rows(&properties, &PropertyDrafts::default(), true);
        assert!(advanced.iter().any(|row| row.key == "rate-limit"));
    }

    #[test]
    fn each_group_is_headed_once() {
        let rows = property_rows(&stored(&[]), &PropertyDrafts::default(), true);
        let headings = rows.iter().filter(|row| !row.heading.is_empty()).count();
        assert_eq!(headings, 5);
        assert!(!rows[0].heading.is_empty());
    }

    #[test]
    fn a_key_the_file_does_not_hold_shows_what_the_server_falls_back_to() {
        let properties = stored(&[]);
        let rows = property_rows(&properties, &PropertyDrafts::default(), false);
        let view = row(&rows, "view-distance");
        assert_eq!(view.text, "");
        assert_eq!(view.placeholder, "10");
        assert!(!view.note.is_empty());
    }

    #[test]
    fn only_edits_that_change_something_are_saved() {
        let properties = stored(&[("motd", "same"), ("pvp", "true")]);
        let mut drafts = PropertyDrafts::default();
        drafts.edit(&properties, "motd", "same");
        drafts.edit(&properties, "view-distance", "");
        assert_eq!(drafts.len(), 0);

        drafts.edit(&properties, "pvp", "false");
        drafts.edit(&properties, "view-distance", "12");
        assert_eq!(
            drafts.values().expect("valid"),
            [
                ("pvp".to_owned(), "false".to_owned()),
                ("view-distance".to_owned(), "12".to_owned()),
            ]
        );
        drafts.edit(&properties, "pvp", "true");
        assert_eq!(drafts.len(), 1);
    }

    #[test]
    fn an_invalid_edit_is_shown_on_its_row_and_blocks_the_save() {
        let properties = stored(&[("view-distance", "10")]);
        let mut drafts = PropertyDrafts::default();
        drafts.edit(&properties, "view-distance", "64");
        let rows = property_rows(&properties, &drafts, false);
        assert!(!row(&rows, "view-distance").error.is_empty());
        assert!(drafts.values().is_none());
    }

    #[test]
    fn a_stored_number_choice_stays_a_number_and_an_unknown_one_is_kept() {
        let properties = stored(&[("difficulty", "1"), ("gamemode", "7")]);
        let rows = property_rows(&properties, &PropertyDrafts::default(), false);
        let difficulty = row(&rows, "difficulty");
        assert_eq!(difficulty.choices, DIFFICULTY_NAMES);
        assert_eq!(difficulty.choice_index, 1);
        let gamemode = row(&rows, "gamemode");
        assert_eq!(gamemode.choices[0], "7");
        assert_eq!(gamemode.choice_index, 0);

        let mut drafts = PropertyDrafts::default();
        drafts.choose(&properties, "difficulty", 3);
        drafts.choose(&properties, "gamemode", 0);
        assert_eq!(
            drafts.values().expect("valid"),
            [("difficulty".to_owned(), "3".to_owned())]
        );
    }

    const DIFFICULTY_NAMES: [&str; 4] = ["peaceful", "easy", "normal", "hard"];

    #[test]
    fn a_choice_the_file_does_not_hold_offers_leaving_it_out() {
        let properties = stored(&[]);
        let rows = property_rows(&properties, &PropertyDrafts::default(), false);
        let difficulty = row(&rows, "difficulty");
        assert_eq!(difficulty.choices.len(), 5);
        assert_eq!(difficulty.choice_index, 0);

        let mut drafts = PropertyDrafts::default();
        drafts.choose(&properties, "difficulty", 4);
        assert_eq!(
            drafts.values().expect("valid"),
            [("difficulty".to_owned(), "3".to_owned())],
            "a number, which every release reads"
        );
        let rows = property_rows(&properties, &drafts, false);
        assert_eq!(row(&rows, "difficulty").choice_index, 4);
        drafts.choose(&properties, "difficulty", 0);
        assert_eq!(drafts.len(), 0);
    }

    #[test]
    fn a_key_saved_while_running_is_marked_until_the_restart() {
        let mut properties = stored(&[("motd", "new")]);
        properties.awaiting_restart.insert("motd".to_owned());
        let rows = property_rows(&properties, &PropertyDrafts::default(), false);
        assert!(row(&rows, "motd").awaiting_restart);
        assert!(!row(&rows, "pvp").awaiting_restart);
    }

    fn defaults(values: &[(&str, &str)]) -> BTreeMap<String, String> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn a_default_can_be_set_changed_and_taken_away() {
        let mut form = ServerDefaultsForm::default();
        form.load(defaults(&[("motd", "Repro"), ("pvp", "false")]));

        form.edit("view-distance", "16");
        form.reset("motd");
        form.choose("difficulty", 4);
        assert_eq!(
            form.values().expect("valid"),
            defaults(&[
                ("difficulty", "hard"),
                ("pvp", "false"),
                ("view-distance", "16")
            ])
        );
        let view = form.view();
        assert_eq!(view.edits, 3);
        let motd = view
            .rows
            .iter()
            .find(|row| row.key == "motd")
            .expect("motd");
        assert!(!motd.can_reset);
        assert!(motd.changed);

        form.choose("difficulty", 0);
        form.edit("view-distance", "");
        form.edit("motd", "Repro");
        assert_eq!(form.view().edits, 0);
    }

    #[test]
    fn unsaved_defaults_survive_a_reload_of_what_is_stored_until_it_changes() {
        let mut form = ServerDefaultsForm::default();
        assert!(form.load(defaults(&[("motd", "Repro")])));
        form.edit("pvp", "false");
        assert!(!form.load(defaults(&[("motd", "Repro")])));
        assert_eq!(form.view().edits, 1);
        assert!(form.load(defaults(&[("motd", "Repro"), ("pvp", "false")])));
        assert_eq!(form.view().edits, 0);
    }

    #[test]
    fn a_default_its_key_refuses_blocks_the_save() {
        let mut form = ServerDefaultsForm::default();
        form.edit("view-distance", "64");
        assert!(form.values().is_none());
        assert!(!form.view().edits_valid);
    }

    #[test]
    fn every_catalog_key_has_a_label_and_a_description() {
        for definition in SERVER_PROPERTY_CATALOG {
            for id in [
                format!("gui-property-{}", definition.key),
                format!("gui-property-{}-detail", definition.key),
            ] {
                assert!(ephervex_i18n::has(&id), "missing {id}");
            }
        }
    }
}
