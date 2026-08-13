//! Typed, preserving management of Minecraft's `ops.json` document.

use std::collections::BTreeMap;

use ephervex_model::OperatorPermissionLevel;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// One operator entry in Minecraft's `ops.json` format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorEntry {
    pub uuid: Uuid,
    pub name: String,
    pub level: OperatorPermissionLevel,
    #[serde(rename = "bypassesPlayerLimit")]
    pub bypasses_player_limit: bool,
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
            level,
            bypasses_player_limit: false,
            unknown: BTreeMap::new(),
        }
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
    /// Existing entries retain `bypassesPlayerLimit` and unknown extension
    /// fields. If a malformed document contains the same UUID more than once,
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
            if entry.name != name || entry.level != level {
                entry.name.clone_from(&name);
                entry.level = level;
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
        assert!(!reparsed.entries()[0].bypasses_player_limit);
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
    fn malformed_or_out_of_range_documents_are_rejected() {
        assert!(OperatorsDocument::parse("not json").is_err());
        assert!(
            OperatorsDocument::parse(
                r#"[{"uuid":"22222222-2222-4222-8222-222222222222","name":"Developer","level":5,"bypassesPlayerLimit":false}]"#
            )
            .is_err()
        );
    }
}
