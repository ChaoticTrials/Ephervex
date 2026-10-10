//! Modrinth's wire shapes, as its API documentation describes them.
//!
//! Declarative on purpose: these types name fields and nothing else. Every
//! judgment about what a field *means* - which file to install, whether an
//! undeclared side is a yes, what an unrecognized relation implies - lives in
//! [`super::mapping`], so the shape of the API and this launcher's reading of
//! it can be reviewed separately.
//!
//! Two of the enums below encode a decision in their `serde` attributes, and
//! both are deliberate:
//!
//! * [`RawSide`] accepts an unrecognized value as `Unknown`, because the API
//!   already has `unknown` as a value an author may leave in place; a fifth
//!   spelling means the same thing - nothing was declared.
//! * [`RawDependencyKind`] refuses one. An unrecognized relation is a claim
//!   this build does not understand, and there is no safe direction to lean.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// A page of search results.
#[derive(Debug, Deserialize)]
pub(super) struct RawSearch {
    #[serde(default)]
    pub(super) hits: Vec<RawHit>,
    #[serde(default)]
    pub(super) total_hits: u64,
}

/// One project as the search endpoint reports it.
#[derive(Debug, Deserialize)]
pub(super) struct RawHit {
    pub(super) project_id: String,
    #[serde(default)]
    pub(super) slug: Option<String>,
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) description: String,
    #[serde(default)]
    pub(super) author: Option<String>,
    #[serde(default)]
    pub(super) client_side: RawSide,
    #[serde(default)]
    pub(super) server_side: RawSide,
    #[serde(default)]
    pub(super) icon_url: Option<String>,
}

/// One project as the project endpoints report it.
#[derive(Debug, Deserialize)]
pub(super) struct RawProject {
    pub(super) id: String,
    #[serde(default)]
    pub(super) slug: Option<String>,
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) description: String,
    /// Required: a project that does not say what it is cannot be shown to be a
    /// mod.
    pub(super) project_type: String,
    #[serde(default)]
    pub(super) client_side: RawSide,
    #[serde(default)]
    pub(super) server_side: RawSide,
    #[serde(default)]
    pub(super) icon_url: Option<String>,
}

/// The four values Modrinth publishes for a side.
///
/// `unknown` is one of them, and it is kept as itself: the author declared
/// nothing, and reading that as `required` would put the mod on a server on the
/// strength of a value that says the opposite of certainty.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum RawSide {
    Required,
    Optional,
    Unsupported,
    #[default]
    #[serde(other)]
    Unknown,
}

/// One version of a project.
#[derive(Debug, Deserialize)]
pub(super) struct RawVersion {
    pub(super) id: String,
    pub(super) project_id: String,
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) version_number: String,
    #[serde(default)]
    pub(super) version_type: RawChannel,
    #[serde(default)]
    pub(super) game_versions: Vec<String>,
    #[serde(default)]
    pub(super) loaders: Vec<String>,
    pub(super) date_published: DateTime<Utc>,
    #[serde(default)]
    pub(super) files: Vec<RawFile>,
    #[serde(default)]
    pub(super) dependencies: Vec<RawDependency>,
    /// The version's own environment, absent on older documents.
    #[serde(default)]
    pub(super) environment: Option<String>,
}

/// One downloadable file belonging to a version.
#[derive(Debug, Deserialize)]
pub(super) struct RawFile {
    #[serde(default)]
    pub(super) hashes: BTreeMap<String, String>,
    pub(super) url: String,
    pub(super) filename: String,
    #[serde(default)]
    pub(super) primary: bool,
    #[serde(default)]
    pub(super) size: Option<u64>,
}

/// How stable a release claims to be.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum RawChannel {
    #[default]
    Release,
    Beta,
    Alpha,
}

/// One relation a version declares against another project.
#[derive(Debug, Deserialize)]
pub(super) struct RawDependency {
    #[serde(default)]
    pub(super) version_id: Option<String>,
    #[serde(default)]
    pub(super) project_id: Option<String>,
    #[serde(default)]
    pub(super) file_name: Option<String>,
    pub(super) dependency_type: RawDependencyKind,
}

/// The four relation types the API documents.
///
/// Deliberately not tolerant of a fifth: an unknown relation is a claim this
/// build does not understand, and guessing which way it leans is how a
/// conflicting mod ends up installed.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum RawDependencyKind {
    Required,
    Optional,
    Incompatible,
    Embedded,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn a_relation_type_this_build_does_not_know_is_refused_rather_than_leaned_one_way() {
        let parsed: Result<RawDependency, _> =
            serde_json::from_str(r#"{"project_id":"P","dependency_type":"suggests"}"#);
        assert!(
            parsed.is_err(),
            "guessing which way an unknown relation leans is how a conflicting mod gets installed"
        );
    }
}
