//! Reading Modrinth's wire shapes as Ephervex's own model.
//!
//! This is where every judgment about what an upstream value *means* lives, and
//! it is the last place Modrinth's vocabulary appears: everything returned from
//! here is a [`crate::model`] type. Keeping the judgments together rather than
//! scattered through the request code is what makes them reviewable as a set -
//! most of them are refusals, and each one is a case where guessing would
//! install the wrong thing.
//!
//! The rules, in one place:
//!
//! * an undeclared side stays undeclared rather than becoming `required`;
//! * a version installs the file marked primary, the first file when none is
//!   marked, and nothing at all when two claim to be;
//! * a file with no verifiable hash is refused, because verification is not
//!   optional;
//! * the strongest published hash comes first, so one artifact has one shared
//!   cache path however it was discovered.

use ephervex_model::{Hash, HashAlgorithm, ModProvider};

use super::PROJECT_TYPE;
use super::dto::{
    RawChannel, RawDependency, RawDependencyKind, RawFile, RawHit, RawProject, RawSide, RawVersion,
};
use crate::error::{ModPlatformError, ModPlatformResult};
use crate::model::{
    DependencyKind, ModDependency, ModFile, ModProject, ModVersionInfo, ReleaseChannel, SideSupport,
};

impl From<RawSide> for SideSupport {
    fn from(value: RawSide) -> Self {
        match value {
            RawSide::Required => Self::Required,
            RawSide::Optional => Self::Optional,
            RawSide::Unsupported => Self::Unsupported,
            RawSide::Unknown => Self::Unknown,
        }
    }
}

impl From<RawChannel> for ReleaseChannel {
    fn from(value: RawChannel) -> Self {
        match value {
            RawChannel::Release => Self::Release,
            RawChannel::Beta => Self::Beta,
            RawChannel::Alpha => Self::Alpha,
        }
    }
}

impl From<RawDependencyKind> for DependencyKind {
    fn from(value: RawDependencyKind) -> Self {
        match value {
            RawDependencyKind::Required => Self::Required,
            RawDependencyKind::Optional => Self::Optional,
            RawDependencyKind::Incompatible => Self::Incompatible,
            RawDependencyKind::Embedded => Self::Embedded,
        }
    }
}

impl From<RawDependency> for ModDependency {
    fn from(value: RawDependency) -> Self {
        Self {
            kind: value.dependency_type.into(),
            project_id: value.project_id,
            version_id: value.version_id,
            file_name: value.file_name,
        }
    }
}

/// One search result as a project.
pub(super) fn hit_project(raw: RawHit) -> ModProject {
    ModProject {
        provider: ModProvider::Modrinth,
        id: raw.project_id,
        slug: raw.slug,
        title: raw.title,
        description: raw.description,
        author: raw.author,
        client_side: raw.client_side.into(),
        server_side: raw.server_side.into(),
        icon_url: icon_url(raw.icon_url),
    }
}

/// One project from a project endpoint.
pub(super) fn project(raw: RawProject) -> ModProject {
    ModProject {
        provider: ModProvider::Modrinth,
        id: raw.id,
        slug: raw.slug,
        title: raw.title,
        description: raw.description,
        // The project endpoint reports team membership rather than a single
        // author. Left empty rather than filled with a guess.
        author: None,
        client_side: raw.client_side.into(),
        server_side: raw.server_side.into(),
        icon_url: icon_url(raw.icon_url),
    }
}

/// The project's icon address, if it published a usable one.
///
/// Modrinth sends `null` for a project with no icon and, for a handful of older
/// projects, an empty string. Both mean the same thing and both become `None`,
/// so nothing downstream has to treat `""` as an address.
fn icon_url(raw: Option<String>) -> Option<String> {
    raw.map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
}

/// One project, refused unless it is actually a mod.
///
/// A modpack is imported rather than installed into an existing instance, and a
/// resource pack is not a mod at all. Both are reachable by typing their slug,
/// so the reference the user gave is named back to them.
pub(super) fn checked_project(raw: RawProject, reference: &str) -> ModPlatformResult<ModProject> {
    if raw.project_type != PROJECT_TYPE {
        return Err(ModPlatformError::ProjectTypeUnsupported {
            reference: reference.to_owned(),
            project_type: raw.project_type,
        });
    }
    Ok(project(raw))
}

/// One version, with the single file this launcher would install.
pub(super) fn version(raw: RawVersion) -> ModPlatformResult<ModVersionInfo> {
    let file = select_primary_file(&raw.id, raw.files)?;
    Ok(ModVersionInfo {
        provider: ModProvider::Modrinth,
        id: raw.id,
        project_id: raw.project_id,
        name: raw.name,
        version_number: raw.version_number,
        channel: raw.version_type.into(),
        game_versions: raw.game_versions,
        loaders: raw.loaders,
        published_at: raw.date_published,
        file,
        dependencies: raw.dependencies.into_iter().map(Into::into).collect(),
    })
}

/// Picks the one file a version installs.
///
/// Modrinth publishes at most one file marked `primary` per version, and the
/// documented fallback when none is marked is the first entry. Two primaries is
/// not a case the format allows, so it is a refusal rather than a coin toss:
/// picking one would install a jar at random, and the wrong jar reproduces the
/// wrong thing.
fn select_primary_file(version_id: &str, files: Vec<RawFile>) -> ModPlatformResult<ModFile> {
    let primaries = files.iter().filter(|file| file.primary).count();
    let chosen = match primaries {
        1 => files.into_iter().find(|file| file.primary),
        0 => files.into_iter().next(),
        found => {
            return Err(ModPlatformError::AmbiguousPrimaryFile {
                version_id: version_id.to_owned(),
                found,
            });
        }
    };

    let chosen = chosen.ok_or_else(|| ModPlatformError::AmbiguousPrimaryFile {
        version_id: version_id.to_owned(),
        found: 0,
    })?;

    let hashes = verifiable_hashes(&chosen);
    if hashes.is_empty() {
        // Verification is mandatory, so a file nobody published a usable hash
        // for is a file this launcher will not fetch.
        return Err(ModPlatformError::HashesMissing {
            version_id: version_id.to_owned(),
        });
    }
    Ok(ModFile {
        file_name: chosen.filename,
        url: chosen.url,
        size: chosen.size,
        hashes,
    })
}

/// The hashes this launcher can verify, strongest first.
///
/// The same order the pack importer uses, so one artifact has one cache path
/// however it was discovered.
fn verifiable_hashes(file: &RawFile) -> Vec<Hash> {
    [
        ("sha512", HashAlgorithm::Sha512),
        ("sha1", HashAlgorithm::Sha1),
    ]
    .into_iter()
    .filter_map(|(name, algorithm)| {
        file.hashes
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .and_then(|(_, digest)| Hash::new(algorithm, digest))
    })
    .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn file(name: &str, primary: bool) -> RawFile {
        RawFile {
            hashes: [(
                "sha1".to_owned(),
                "da39a3ee5e6b4b0d3255bfef95601890afd80709".to_owned(),
            )]
            .into_iter()
            .collect(),
            url: format!("https://cdn.modrinth.com/data/x/{name}"),
            filename: name.to_owned(),
            primary,
            size: Some(1),
        }
    }

    #[test]
    fn the_file_marked_primary_is_the_one_installed() {
        let chosen = select_primary_file(
            "v1",
            vec![
                file("sources.jar", false),
                file("mod.jar", true),
                file("javadoc.jar", false),
            ],
        )
        .expect("one primary");
        assert_eq!(chosen.file_name, "mod.jar");
    }

    #[test]
    fn with_no_primary_marked_the_first_file_is_used_as_the_api_specifies() {
        let chosen =
            select_primary_file("v1", vec![file("mod.jar", false), file("extra.jar", false)])
                .expect("the documented fallback");
        assert_eq!(chosen.file_name, "mod.jar");
    }

    #[test]
    fn two_primaries_are_refused_rather_than_picked_between() {
        // The format allows at most one. Guessing would install a jar at
        // random, and the wrong jar reproduces the wrong thing.
        let error = select_primary_file("v1", vec![file("a.jar", true), file("b.jar", true)])
            .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::AmbiguousPrimaryFile { found: 2, .. }
        ));
    }

    #[test]
    fn a_version_with_no_file_is_refused() {
        let error = select_primary_file("v1", Vec::new()).expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::AmbiguousPrimaryFile { found: 0, .. }
        ));
    }

    #[test]
    fn a_file_with_no_verifiable_hash_is_refused_because_verification_is_mandatory() {
        let mut entry = file("mod.jar", true);
        entry.hashes = [(
            "md5".to_owned(),
            "0123456789abcdef0123456789abcdef".to_owned(),
        )]
        .into_iter()
        .collect();
        let error = select_primary_file("v1", vec![entry]).expect_err("must refuse");
        assert!(matches!(error, ModPlatformError::HashesMissing { .. }));
    }

    #[test]
    fn the_strongest_published_hash_comes_first() {
        let mut entry = file("mod.jar", true);
        entry.hashes.insert("sha512".to_owned(), "c".repeat(128));
        let hashes = verifiable_hashes(&entry);
        assert_eq!(hashes[0].algorithm, HashAlgorithm::Sha512);
        assert_eq!(hashes[1].algorithm, HashAlgorithm::Sha1);
    }

    #[test]
    fn an_undeclared_side_is_read_as_undeclared() {
        let raw: RawProject = serde_json::from_str(
            r#"{"id":"AABBCCDD","title":"X","description":"","project_type":"mod",
                "client_side":"required","server_side":"unknown"}"#,
        )
        .expect("parse");
        let project = project(raw);
        assert_eq!(project.client_side, SideSupport::Required);
        assert_eq!(
            project.server_side,
            SideSupport::Unknown,
            "`unknown` is a value the author chose, not a missing field to fill in"
        );
    }

    #[test]
    fn a_side_value_this_build_has_never_seen_is_undeclared_rather_than_assumed() {
        let raw: RawProject = serde_json::from_str(
            r#"{"id":"AABBCCDD","title":"X","description":"","project_type":"mod",
                "client_side":"required","server_side":"something-new"}"#,
        )
        .expect("parse");
        assert_eq!(project(raw).server_side, SideSupport::Unknown);
    }

    #[test]
    fn every_documented_dependency_type_maps_to_a_relation() {
        for (raw, expected) in [
            ("required", DependencyKind::Required),
            ("optional", DependencyKind::Optional),
            ("incompatible", DependencyKind::Incompatible),
            ("embedded", DependencyKind::Embedded),
        ] {
            let parsed: RawDependency = serde_json::from_str(&format!(
                r#"{{"project_id":"P","dependency_type":"{raw}"}}"#
            ))
            .expect("parse");
            assert_eq!(ModDependency::from(parsed).kind, expected);
        }
    }
}
