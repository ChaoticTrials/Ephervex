//! Builders shared by this crate's own tests.
//!
//! Compiled only under `cfg(test)`: these exist so a resolver test can state
//! the graph it is about in three lines instead of thirty, not to widen the
//! crate's public surface.
#![allow(clippy::expect_used)]

use chrono::{DateTime, TimeZone as _, Utc};
use ephervex_model::{Hash, HashAlgorithm, ModProvider, ModRelationKind};

use crate::model::{
    ModDependency, ModFile, ModProject, ModVersionInfo, ReleaseChannel, SideSupport,
};

/// A digest that is well-formed but means nothing. Tests here never hash bytes.
const SHA1: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";

/// A fixed publication time, offset by minutes so ordering is explicit.
pub(crate) fn published(minutes: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
        .single()
        .expect("a valid timestamp")
        + chrono::TimeDelta::minutes(minutes)
}

/// A version with no dependencies, published at the epoch used above.
pub(crate) fn version(
    id: &str,
    project_id: &str,
    game_versions: &[&str],
    loaders: &[&str],
) -> ModVersionInfo {
    ModVersionInfo {
        provider: ModProvider::Modrinth,
        id: id.to_owned(),
        project_id: project_id.to_owned(),
        name: format!("{project_id} {id}"),
        version_number: id.to_owned(),
        channel: ReleaseChannel::Release,
        game_versions: game_versions
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        loaders: loaders.iter().map(|value| (*value).to_owned()).collect(),
        published_at: published(0),
        file: ModFile {
            file_name: format!("{project_id}-{id}.jar"),
            url: format!("https://cdn.modrinth.com/data/{project_id}/{id}.jar"),
            size: Some(16),
            hashes: vec![Hash::new(HashAlgorithm::Sha1, SHA1).expect("valid sha1")],
        },
        dependencies: Vec::new(),
    }
}

/// A version compatible with the target every test uses.
pub(crate) fn compatible(id: &str, project_id: &str) -> ModVersionInfo {
    version(id, project_id, &["1.20.1"], &["neoforge"])
}

/// Marks when a version was published, which is what "newest" means.
pub(crate) fn at(mut version: ModVersionInfo, minutes: i64) -> ModVersionInfo {
    version.published_at = published(minutes);
    version
}

/// Adds a relation naming a project.
pub(crate) fn needs(
    mut version: ModVersionInfo,
    kind: ModRelationKind,
    project_id: &str,
) -> ModVersionInfo {
    version.dependencies.push(ModDependency {
        kind,
        project_id: Some(project_id.to_owned()),
        version_id: None,
        file_name: None,
    });
    version
}

/// Adds a relation pinning an exact version.
pub(crate) fn pins(
    mut version: ModVersionInfo,
    kind: ModRelationKind,
    version_id: &str,
) -> ModVersionInfo {
    version.dependencies.push(ModDependency {
        kind,
        project_id: None,
        version_id: Some(version_id.to_owned()),
        file_name: None,
    });
    version
}

/// Adds a relation naming only a bare file.
pub(crate) fn needs_file(
    mut version: ModVersionInfo,
    kind: ModRelationKind,
    file_name: &str,
) -> ModVersionInfo {
    version.dependencies.push(ModDependency {
        kind,
        project_id: None,
        version_id: None,
        file_name: Some(file_name.to_owned()),
    });
    version
}

/// A project that declares itself required on both sides.
pub(crate) fn project(id: &str) -> ModProject {
    ModProject {
        provider: ModProvider::Modrinth,
        id: id.to_owned(),
        slug: Some(id.to_ascii_lowercase()),
        title: id.to_owned(),
        description: format!("{id}, for testing"),
        author: Some("someone".to_owned()),
        client_side: SideSupport::Required,
        server_side: SideSupport::Required,
        icon_url: None,
    }
}
