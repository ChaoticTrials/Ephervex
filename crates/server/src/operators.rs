//! Typed, preserving management of Minecraft's `ops.json` document.

use std::collections::BTreeMap;

use ephervex_model::OperatorPermissionLevel;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::statistics::bounded_display_name;

/// How many operators one listing carries.
pub const MAX_LISTED_OPERATORS: usize = 200;

/// One operator entry in Minecraft's `ops.json` format.
///
/// Minecraft reads `level` and `bypassesPlayerLimit` as optional and accepts any
/// integer level, so both are kept as read: an entry is rewritten only in the
/// fields Ephervex sets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorEntry {
    pub uuid: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    level: Option<Value>,
    #[serde(
        default,
        rename = "bypassesPlayerLimit",
        skip_serializing_if = "Option::is_none"
    )]
    bypasses_player_limit: Option<Value>,
    /// Fields written by a newer Minecraft build or server implementation.
    #[serde(flatten)]
    unknown: BTreeMap<String, Value>,
}

impl OperatorEntry {
    #[must_use]
    pub fn new(uuid: Uuid, name: impl Into<String>, level: OperatorPermissionLevel) -> Self {
        Self {
            uuid,
            name: name.into(),
            level: Some(Value::from(level.value())),
            bypasses_player_limit: Some(Value::Bool(false)),
            unknown: BTreeMap::new(),
        }
    }

    /// The entry's permission level, or `None` when it is absent or not an integer.
    #[must_use]
    pub fn level(&self) -> Option<i64> {
        self.level.as_ref().and_then(Value::as_i64)
    }

    /// Whether the entry bypasses the player limit, or `None` when it is absent
    /// or not a boolean.
    #[must_use]
    pub fn bypasses_player_limit(&self) -> Option<bool> {
        self.bypasses_player_limit.as_ref().and_then(Value::as_bool)
    }
}

/// The result of applying one desired operator to a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorUpdate {
    Inserted,
    Updated,
    Unchanged,
}

/// One element of the `ops.json` array.
///
/// An object is an operator only with a canonical hyphenated `uuid` and a string
/// `name`. Minecraft skips an object without a usable `uuid` or `name` and loads
/// the rest of the file, so any other object is kept exactly as read and never
/// matched or rewritten.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
enum Element {
    Operator(OperatorEntry),
    Unrecognized(Value),
}

impl Element {
    fn classify(value: Value) -> Self {
        if !is_operator_object(&value) {
            return Self::Unrecognized(value);
        }
        match serde_json::from_value::<OperatorEntry>(value.clone()) {
            Ok(entry) => Self::Operator(entry),
            Err(_) => Self::Unrecognized(value),
        }
    }
}

/// Whether an element has the `uuid` and `name` Minecraft requires.
///
/// Java's `UUID.fromString` takes the hyphenated 36-character form, while the
/// `uuid` crate also reads simple, braced, and URN forms that Minecraft skips.
fn is_operator_object(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let canonical_uuid = object
        .get("uuid")
        .and_then(Value::as_str)
        .is_some_and(|text| text.len() == 36 && Uuid::parse_str(text).is_ok());
    canonical_uuid && object.get("name").is_some_and(Value::is_string)
}

/// A parsed `ops.json` array.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OperatorsDocument {
    elements: Vec<Element>,
}

impl OperatorsDocument {
    /// Parses an existing `ops.json` document.
    ///
    /// Elements Minecraft would skip are kept untouched. The text must still be
    /// a JSON array of objects, which is what Minecraft's reader requires.
    pub fn parse(text: &str) -> Result<Self, OperatorsDocumentError> {
        let values: Vec<Value> =
            serde_json::from_str(text).map_err(OperatorsDocumentError::Invalid)?;
        if let Some(index) = values.iter().position(|value| !value.is_object()) {
            return Err(OperatorsDocumentError::NotAnObject { index });
        }
        let elements = values.into_iter().map(Element::classify).collect();
        Ok(Self { elements })
    }

    /// Serializes a complete `ops.json` document using JSON escaping.
    ///
    /// Indented on purpose, unlike launcher-owned storage: `ops.json` belongs to
    /// the Minecraft server, which writes it indented itself and whose operators
    /// routinely edit it by hand. Rewriting it compactly would fight both.
    pub fn render(&self) -> Result<String, OperatorsDocumentError> {
        let mut rendered = serde_json::to_string_pretty(&self.elements)
            .map_err(OperatorsDocumentError::Serialization)?;
        rendered.push('\n');
        Ok(rendered)
    }

    /// The operator entries, in file order, without the elements Minecraft skips.
    pub fn entries(&self) -> impl Iterator<Item = &OperatorEntry> {
        self.elements.iter().filter_map(|element| match element {
            Element::Operator(entry) => Some(entry),
            Element::Unrecognized(_) => None,
        })
    }

    /// Adds or updates the operator identified by `uuid`.
    ///
    /// Existing entries retain `bypassesPlayerLimit`, including its absence, and
    /// unknown extension fields. If a malformed document contains the same UUID more than once,
    /// every matching entry is made consistent without disturbing the rest of
    /// the array.
    pub fn upsert(
        &mut self,
        uuid: Uuid,
        name: impl Into<String>,
        level: OperatorPermissionLevel,
    ) -> OperatorUpdate {
        let name = name.into();
        let mut found = false;
        let mut changed = false;
        for entry in self
            .elements
            .iter_mut()
            .filter_map(|element| match element {
                Element::Operator(entry) if entry.uuid == uuid => Some(entry),
                _ => None,
            })
        {
            found = true;
            if entry.name != name || entry.level() != Some(i64::from(level.value())) {
                entry.name.clone_from(&name);
                entry.level = Some(Value::from(level.value()));
                changed = true;
            }
        }
        if !found {
            self.elements
                .push(Element::Operator(OperatorEntry::new(uuid, name, level)));
            OperatorUpdate::Inserted
        } else if changed {
            OperatorUpdate::Updated
        } else {
            OperatorUpdate::Unchanged
        }
    }
}

/// One operator as shown to a user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedOperator {
    /// The entry's name, shortened and stripped of control characters.
    pub name: String,
    /// The entry's permission level, or `None` when it has no integer level.
    pub level: Option<i64>,
}

/// The operators of an `ops.json` document, bounded for display.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OperatorListing {
    /// The first [`MAX_LISTED_OPERATORS`] operators, in file order.
    pub operators: Vec<ListedOperator>,
    /// How many operators the document holds beyond those listed.
    pub omitted: usize,
}

impl OperatorsDocument {
    /// The document's operators for display, without changing the document.
    #[must_use]
    pub fn listing(&self) -> OperatorListing {
        let mut entries = self.entries();
        let operators: Vec<ListedOperator> = entries
            .by_ref()
            .take(MAX_LISTED_OPERATORS)
            .map(|entry| ListedOperator {
                name: bounded_display_name(&entry.name),
                level: entry.level(),
            })
            .collect();
        OperatorListing {
            operators,
            omitted: entries.count(),
        }
    }
}

/// A malformed or unserializable `ops.json` document.
#[derive(Debug, thiserror::Error)]
pub enum OperatorsDocumentError {
    #[error("the Minecraft operator list is not valid JSON")]
    Invalid(#[source] serde_json::Error),
    #[error("element {index} of the Minecraft operator list is not an object")]
    NotAnObject { index: usize },
    #[error("the Minecraft operator list could not be serialized")]
    Serialization(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn uuid(value: &str) -> Uuid {
        Uuid::parse_str(value).expect("uuid")
    }

    #[test]
    fn a_listing_is_bounded_and_leaves_out_entries_minecraft_skips() {
        let mut entries: Vec<String> = (0..MAX_LISTED_OPERATORS + 3)
            .map(|index| {
                format!(
                    r#"{{"uuid":"00000000-0000-4000-8000-{index:012}","name":"Op\n{index}","level":{}}}"#,
                    index % 5
                )
            })
            .collect();
        entries.push(r#"{"name":"NoUuid","level":4}"#.to_owned());
        entries.push(
            r#"{"uuid":"ffffffff-ffff-4fff-8fff-ffffffffffff","name":"Unleveled"}"#.to_owned(),
        );
        let document =
            OperatorsDocument::parse(&format!("[{}]", entries.join(","))).expect("parse");

        let listing = document.listing();

        assert_eq!(listing.operators.len(), MAX_LISTED_OPERATORS);
        assert_eq!(listing.omitted, 4);
        assert_eq!(listing.operators[1].name, "Op 1");
        assert_eq!(listing.operators[1].level, Some(1));
        assert!(
            listing
                .operators
                .iter()
                .all(|operator| operator.name != "NoUuid")
        );
    }

    #[test]
    fn upsert_preserves_unrelated_operators_and_extension_fields() {
        let unrelated = uuid("11111111-1111-4111-8111-111111111111");
        let selected = uuid("22222222-2222-4222-8222-222222222222");
        let mut document = OperatorsDocument::parse(&format!(
            r#"[
  {{"uuid":"{unrelated}","name":"Existing","level":2,"bypassesPlayerLimit":true,"modField":{{"keep":7}}}},
  {{"uuid":"{selected}","name":"OldName","level":1,"bypassesPlayerLimit":true,"futureFlag":"keep"}}
]"#
        ))
        .expect("parse");

        assert_eq!(
            document.upsert(
                selected,
                "Developer",
                OperatorPermissionLevel::new(4).expect("level")
            ),
            OperatorUpdate::Updated
        );

        let rendered = document.render().expect("render");
        let value: Value = serde_json::from_str(&rendered).expect("valid JSON");
        let entries = value.as_array().expect("array");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["uuid"], unrelated.to_string());
        assert_eq!(entries[0]["modField"]["keep"], 7);
        assert_eq!(entries[1]["name"], "Developer");
        assert_eq!(entries[1]["level"], 4);
        assert_eq!(entries[1]["bypassesPlayerLimit"], true);
        assert_eq!(entries[1]["futureFlag"], "keep");
    }

    #[test]
    fn inserted_names_are_json_escaped_instead_of_concatenated() {
        let selected = uuid("22222222-2222-4222-8222-222222222222");
        let mut document = OperatorsDocument::default();
        assert_eq!(
            document.upsert(
                selected,
                "quoted\"name\\value",
                OperatorPermissionLevel::new(3).expect("level")
            ),
            OperatorUpdate::Inserted
        );

        let rendered = document.render().expect("render");
        let reparsed = OperatorsDocument::parse(&rendered).expect("reparse");
        assert_eq!(
            reparsed.entries().next().expect("entry").name,
            "quoted\"name\\value"
        );
        assert_eq!(
            reparsed
                .entries()
                .next()
                .expect("entry")
                .bypasses_player_limit(),
            Some(false)
        );
    }

    #[test]
    fn an_identical_operator_is_left_unchanged() {
        let selected = uuid("22222222-2222-4222-8222-222222222222");
        let mut document = OperatorsDocument::default();
        let level = OperatorPermissionLevel::new(4).expect("level");
        assert_eq!(
            document.upsert(selected, "Developer", level),
            OperatorUpdate::Inserted
        );
        assert_eq!(
            document.upsert(selected, "Developer", level),
            OperatorUpdate::Unchanged
        );
    }

    #[test]
    fn non_json_documents_are_rejected() {
        assert!(OperatorsDocument::parse("not json").is_err());
    }

    #[test]
    fn entries_minecraft_accepts_survive_parse_and_render_unchanged() {
        let id = "33333333-3333-4333-8333-333333333333";
        let text = format!(
            r#"[{{"uuid":"{id}","name":"Zero","level":0}},{{"uuid":"{id}","name":"NoLevel","bypassesPlayerLimit":true}},{{"uuid":"{id}","name":"Five","level":5,"bypassesPlayerLimit":false}}]"#
        );
        let document = OperatorsDocument::parse(&text).expect("parse");
        let entries: Vec<_> = document.entries().collect();
        assert_eq!(entries[0].level(), Some(0));
        assert_eq!(entries[0].bypasses_player_limit(), None);
        assert_eq!(entries[1].level(), None);
        assert_eq!(entries[2].level(), Some(5));

        let rendered: Value =
            serde_json::from_str(&document.render().expect("render")).expect("valid JSON");
        let original: Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(rendered, original);
    }

    #[test]
    fn upsert_sets_a_valid_level_on_an_entry_without_one_and_keeps_absent_fields_absent() {
        let selected = uuid("22222222-2222-4222-8222-222222222222");
        let mut document = OperatorsDocument::parse(&format!(
            r#"[{{"uuid":"{selected}","name":"Developer","level":0}}]"#
        ))
        .expect("parse");

        assert_eq!(
            document.upsert(
                selected,
                "Developer",
                OperatorPermissionLevel::new(4).expect("level")
            ),
            OperatorUpdate::Updated
        );

        let rendered: Value =
            serde_json::from_str(&document.render().expect("render")).expect("valid JSON");
        assert_eq!(rendered[0]["level"], 4);
        assert!(rendered[0].get("bypassesPlayerLimit").is_none());
    }

    const SKIPPED_ELEMENTS: &str = r#"[
  {"name":"NoUuid","level":4},
  {"uuid":"44444444-4444-4444-8444-444444444444","level":4},
  {"uuid":"not-a-uuid","name":"BadUuid"},
  {"uuid":"55555555555555555555555555555555","name":"SimpleUuid"},
  {"uuid":"{66666666-6666-4666-8666-666666666666}","name":"BracedUuid"},
  {"uuid":"77777777-7777-4777-8777-777777777777","name":7},
  {"uuid":12,"name":"NumericUuid"},
  {}
]"#;

    #[test]
    fn elements_minecraft_skips_are_accepted_and_survive_render_unchanged() {
        let document = OperatorsDocument::parse(SKIPPED_ELEMENTS).expect("parse");
        assert_eq!(document.entries().count(), 0);

        let rendered: Value =
            serde_json::from_str(&document.render().expect("render")).expect("valid JSON");
        let original: Value = serde_json::from_str(SKIPPED_ELEMENTS).expect("valid JSON");
        assert_eq!(rendered, original);
    }

    #[test]
    fn upsert_ignores_skipped_elements_and_keeps_their_position() {
        let selected = uuid("22222222-2222-4222-8222-222222222222");
        let text = format!(
            r#"[{{"name":"NoUuid"}},{{"uuid":"{selected}","name":"Developer","level":1}},{{"uuid":"22222222222222222222222222222222","name":"SimpleForm"}}]"#
        );
        let mut document = OperatorsDocument::parse(&text).expect("parse");

        assert_eq!(
            document.upsert(
                selected,
                "Developer",
                OperatorPermissionLevel::new(4).expect("level")
            ),
            OperatorUpdate::Updated
        );

        let rendered: Value =
            serde_json::from_str(&document.render().expect("render")).expect("valid JSON");
        assert_eq!(rendered.as_array().expect("array").len(), 3);
        assert_eq!(rendered[0], serde_json::json!({"name": "NoUuid"}));
        assert_eq!(rendered[1]["level"], 4);
        assert_eq!(
            rendered[2],
            serde_json::json!({"uuid": "22222222222222222222222222222222", "name": "SimpleForm"})
        );
    }

    #[test]
    fn upsert_appends_the_operator_after_skipped_elements() {
        let selected = uuid("22222222-2222-4222-8222-222222222222");
        let mut document = OperatorsDocument::parse(SKIPPED_ELEMENTS).expect("parse");

        assert_eq!(
            document.upsert(
                selected,
                "Developer",
                OperatorPermissionLevel::new(4).expect("level")
            ),
            OperatorUpdate::Inserted
        );

        let rendered: Value =
            serde_json::from_str(&document.render().expect("render")).expect("valid JSON");
        assert_eq!(rendered.as_array().expect("array").len(), 9);
        assert_eq!(rendered[8]["uuid"], selected.to_string());
    }

    #[test]
    fn documents_that_are_not_an_array_of_objects_are_rejected() {
        assert!(OperatorsDocument::parse(r#"{"uuid":"x"}"#).is_err());
        assert!(matches!(
            OperatorsDocument::parse(r#"[{"name":"A"}, 3]"#),
            Err(OperatorsDocumentError::NotAnObject { index: 1 })
        ));
        assert!(OperatorsDocument::parse("[null]").is_err());
    }
}
