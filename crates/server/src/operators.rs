//! Typed, preserving management of Minecraft's `ops.json` document.

use std::collections::BTreeMap;

use ephervex_model::OperatorPermissionLevel;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

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

/// A parsed `ops.json` array.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OperatorsDocument {
    entries: Vec<OperatorEntry>,
}

impl OperatorsDocument {
    /// Parses an existing `ops.json` document.
    pub fn parse(text: &str) -> Result<Self, OperatorsDocumentError> {
        let entries = serde_json::from_str(text).map_err(OperatorsDocumentError::Invalid)?;
        Ok(Self { entries })
    }

    /// Serializes a complete `ops.json` document using JSON escaping.
    ///
    /// Indented on purpose, unlike launcher-owned storage: `ops.json` belongs to
    /// the Minecraft server, which writes it indented itself and whose operators
    /// routinely edit it by hand. Rewriting it compactly would fight both.
    pub fn render(&self) -> Result<String, OperatorsDocumentError> {
        let mut rendered = serde_json::to_string_pretty(&self.entries)
            .map_err(OperatorsDocumentError::Serialization)?;
        rendered.push('\n');
        Ok(rendered)
    }

    #[must_use]
    pub fn entries(&self) -> &[OperatorEntry] {
        &self.entries
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
        for entry in self.entries.iter_mut().filter(|entry| entry.uuid == uuid) {
            found = true;
            if entry.name != name || entry.level() != Some(i64::from(level.value())) {
                entry.name.clone_from(&name);
                entry.level = Some(Value::from(level.value()));
                changed = true;
            }
        }
        if !found {
            self.entries.push(OperatorEntry::new(uuid, name, level));
            OperatorUpdate::Inserted
        } else if changed {
            OperatorUpdate::Updated
        } else {
            OperatorUpdate::Unchanged
        }
    }
}

/// A malformed or unserializable `ops.json` document.
#[derive(Debug, thiserror::Error)]
pub enum OperatorsDocumentError {
    #[error("the Minecraft operator list is not valid JSON")]
    Invalid(#[source] serde_json::Error),
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
        assert_eq!(reparsed.entries()[0].name, "quoted\"name\\value");
        assert_eq!(reparsed.entries()[0].bypasses_player_limit(), Some(false));
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
        assert_eq!(document.entries()[0].level(), Some(0));
        assert_eq!(document.entries()[0].bypasses_player_limit(), None);
        assert_eq!(document.entries()[1].level(), None);
        assert_eq!(document.entries()[2].level(), Some(5));

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
}
