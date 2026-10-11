//! Reading CurseForge's wire shapes as Ephervex's own model.
//!
//! This is the last place CurseForge's vocabulary appears. Everything returned
//! from here is a [`crate::model`] type, so nothing above this module sees a
//! `classId`, a `relationType`, or a `modLoaderType`.
//!
//! The judgments, in one place, because most of them are refusals:
//!
//! * a project this build cannot confirm is a Mod is never treated as one;
//! * a file with no third-party download address keeps every fact the API
//!   stated about it and records that its bytes may not be fetched; dropping
//!   it would offer a *different* release than the newest one that fits, and
//!   refusing it would throw away metadata CurseForge did supply;
//! * a file with no verifiable digest is refused, because verification is not
//!   optional;
//! * a download address is checked against the hosts CurseForge delivers from
//!   before it can name a machine this launcher connects to;
//! * a relation whose type this build does not act on is left out rather than
//!   promoted to a requirement;
//! * CurseForge states client and server support per file, not per project, as
//!   `Client`/`Server` entries folded into a file's `gameVersions`; a project
//!   is read as supporting a side when one of its own recent files declares
//!   it, and stays undeclared only when none of them names either side.

use chrono::{DateTime, Utc};
use ephervex_download::DomainAllowlist;
use ephervex_model::{CurseForgeFileId, CurseForgeProjectId, ModProvider, ModRelationKind};

use super::api::{
    CURSEFORGE_CLIENT_ENVIRONMENT, CURSEFORGE_ENVIRONMENT_NAMES, CURSEFORGE_SERVER_ENVIRONMENT,
    CurseForgeProjectClasses, DOCUMENTED_LOADER_NAMES, supported_hashes,
};
use super::dto::{
    CurseForgeFileDocument, CurseForgeFileRelation, CurseForgeProject, CurseForgeProjectFile,
    RELATION_EMBEDDED_LIBRARY, RELATION_INCLUDE, RELATION_INCOMPATIBLE,
    RELATION_OPTIONAL_DEPENDENCY, RELATION_REQUIRED_DEPENDENCY, RELEASE_TYPE_BETA,
    RELEASE_TYPE_RELEASE,
};
use crate::error::{ModPlatformError, ModPlatformResult};
use crate::model::{
    Distribution, ModDependency, ModFile, ModProject, ModVersionInfo, NotModProject, ProjectBatch,
    ReleaseChannel, SideSupport,
};

/// One project, once its class has been confirmed to be Mods.
///
/// The class check is not a formality and not something the search filter is
/// taken as proof of: `classId` travels on the project document itself, and it
/// is checked against the classes the provider named in this same operation.
pub(super) fn checked_project(
    raw: CurseForgeProject,
    classes: &CurseForgeProjectClasses,
) -> ModPlatformResult<ModProject> {
    if let Some(rejection) = classes.rejection_for(&raw) {
        return Err(rejection);
    }
    Ok(project(raw))
}

/// Files one project of a batch under the mods or the non-mods.
///
/// Only a project confirmed to be in another class is a non-mod. One whose
/// class cannot be determined is still a failure, as it is for a single lookup.
pub(super) fn sort_project(
    batch: &mut ProjectBatch,
    raw: CurseForgeProject,
    classes: &CurseForgeProjectClasses,
) -> ModPlatformResult<()> {
    match classes.rejection_for(&raw) {
        None => batch.mods.push(project(raw)),
        Some(ModPlatformError::ProjectTypeUnsupported {
            reference,
            project_type,
        }) => batch.not_mods.push(NotModProject {
            id: reference,
            project_type,
        }),
        Some(error) => return Err(error),
    }
    Ok(())
}

/// One confirmed Mods project as the neutral model.
fn project(raw: CurseForgeProject) -> ModProject {
    let (client_side, server_side) = declared_sides(&raw.latest_files);
    ModProject {
        provider: ModProvider::CurseForge,
        id: raw.id.to_string(),
        slug: non_empty(Some(raw.slug)),
        title: raw.name,
        description: raw.summary,
        // The first credited author. CurseForge reports a list and Ephervex's
        // model shows one line; taking the first is a display decision, and
        // nothing downstream reads it.
        author: raw
            .authors
            .into_iter()
            .map(|author| author.name)
            .find(|name| !name.trim().is_empty()),
        client_side,
        server_side,
        icon_url: raw.logo.and_then(|logo| non_empty(logo.url)),
    }
}

/// What a project's own recent files say about which side(s) they run on.
///
/// CurseForge states this per file, folded into the same `gameVersions` array
/// that carries Minecraft releases and loader names: a `Client` or `Server`
/// entry is the file's own claim. A project is read as supporting a side when
/// any of its listed files declares it; a project none of whose files names
/// either side is undeclared rather than assumed to run everywhere, because
/// naming neither is not the same claim as naming both.
fn declared_sides(files: &[CurseForgeProjectFile]) -> (SideSupport, SideSupport) {
    sides_from_entries(files.iter().flat_map(|file| file.game_versions.iter()))
}

/// What one exact file's own `gameVersions` says about which side(s) it runs
/// on.
///
/// The single-file counterpart to [`declared_sides`]: used wherever the exact
/// release is already known, such as a pack import resolving one pinned
/// CurseForge file, rather than a project consulted with no particular
/// release in mind.
pub(super) fn file_declared_sides(game_versions: &[String]) -> (SideSupport, SideSupport) {
    sides_from_entries(game_versions.iter())
}

/// The shared read behind [`declared_sides`] and [`file_declared_sides`]:
/// a side is `Required` when some entry names it, `Unsupported` when entries
/// exist but never name it, and both stay `Unknown` when nothing names either
/// side at all.
fn sides_from_entries<'a>(entries: impl Iterator<Item = &'a String>) -> (SideSupport, SideSupport) {
    let mut client = false;
    let mut server = false;
    for entry in entries {
        let entry = entry.trim();
        if entry.eq_ignore_ascii_case(CURSEFORGE_CLIENT_ENVIRONMENT) {
            client = true;
        } else if entry.eq_ignore_ascii_case(CURSEFORGE_SERVER_ENVIRONMENT) {
            server = true;
        }
    }
    if !client && !server {
        return (SideSupport::Unknown, SideSupport::Unknown);
    }
    let side = |declared: bool| {
        if declared {
            SideSupport::Required
        } else {
            SideSupport::Unsupported
        }
    };
    (side(client), side(server))
}

/// One file as an installable version.
///
/// `hosts` is the allowlist the download address must satisfy. It is applied
/// here, at the provider boundary, so a version that reaches the resolver is
/// one the launcher is actually willing to fetch.
pub(super) fn version(
    document: CurseForgeFileDocument,
    hosts: &DomainAllowlist,
) -> ModPlatformResult<ModVersionInfo> {
    let project = CurseForgeProjectId::new(document.mod_id).ok_or_else(|| invalid_id("modId"))?;
    let file = CurseForgeFileId::new(document.id).ok_or_else(|| invalid_id("id"))?;

    if !document.is_available {
        return Err(ModPlatformError::FileUnavailable {
            provider: ModProvider::CurseForge,
            project_id: project.to_string(),
            file_id: file.to_string(),
        });
    }

    let hashes = supported_hashes(&document.hashes);
    let distribution = distribution(&document, hosts)?;

    // Verification is not optional for anything this launcher installs, and a
    // restricted file needs a digest twice over: it is the only way to
    // recognize the copy the user fetches by hand.
    if hashes.is_empty() {
        return Err(ModPlatformError::HashesMissing {
            version_id: file.to_string(),
        });
    }

    let (game_versions, loaders) = split_declared_support(&document.game_versions);
    let (client_side, server_side) = file_declared_sides(&document.game_versions);
    let dependencies = file_relations(&document.dependencies)?;
    // CurseForge publishes one label for a release and no separate version
    // number. Both fields carry it rather than one of them carrying something
    // parsed out of the other.
    let label = document
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(document.file_name.as_str())
        .to_owned();

    Ok(ModVersionInfo {
        provider: ModProvider::CurseForge,
        id: file.to_string(),
        project_id: project.to_string(),
        name: label.clone(),
        version_number: label,
        channel: channel(document.release_type),
        game_versions,
        loaders,
        published_at: published_at(document.file_date.as_deref()),
        file: ModFile {
            file_name: document.file_name,
            distribution,
            size: document.file_length,
            hashes,
        },
        dependencies,
        client_side,
        server_side,
    })
}

/// Whether this launcher may fetch a file's bytes, and from where.
///
/// Null, absent, or blank all mean the same thing: the project's owner turned
/// off third-party distribution. That is recorded rather than refused - the
/// release is real, its metadata is real, and only its acquisition changes -
/// and no address is assembled out of the ids to route around it.
///
/// An address the provider did publish decides which machine the launcher
/// connects to, so it is checked against the hosts CurseForge delivers from
/// like any other remote address.
pub(super) fn distribution(
    document: &CurseForgeFileDocument,
    hosts: &DomainAllowlist,
) -> ModPlatformResult<Distribution> {
    let Some(address) = document
        .download_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    else {
        return Ok(Distribution::Restricted);
    };
    hosts
        .validate(address)
        .map(|url| Distribution::Download(url.to_string()))
        .map_err(|source| ModPlatformError::DownloadAddressRejected {
            provider: ModProvider::CurseForge,
            detail: source.to_string(),
        })
}

/// What one file document says about other projects.
///
/// Shared by the mod browser and by pack import, which read the same document
/// and must agree about what it declared.
///
/// A relation kind this build does not act on is skipped on purpose. A relation
/// of a recognized kind whose target names nothing is not: dropping it would
/// report that no such relation exists, and a lost requirement or
/// incompatibility removes a guard from the plan. The document is refused.
pub(super) fn file_relations(
    relations: &[CurseForgeFileRelation],
) -> ModPlatformResult<Vec<ModDependency>> {
    let mut dependencies = Vec::new();
    for relation in relations {
        let Some(kind) = dependency_kind(relation.relation_type) else {
            continue;
        };
        let project = CurseForgeProjectId::new(relation.mod_id).ok_or_else(|| {
            ModPlatformError::ResponseInvalid {
                provider: ModProvider::CurseForge,
                detail: "a file document declared a relation to a project with id 0, which names nothing"
                    .to_owned(),
            }
        })?;
        dependencies.push(ModDependency {
            kind,
            project_id: Some(project.to_string()),
            // CurseForge states relations project to project. There is no
            // file to pin and none is invented: the dependency is resolved
            // against this instance's own exact target, the same way the
            // mod that declared it was.
            version_id: None,
            file_name: None,
        });
    }
    Ok(dependencies)
}

/// Splits one `gameVersions` array into Minecraft releases and loader names.
///
/// CurseForge mixes both, plus environment entries that are neither. Anything
/// matching a name from the API's own documented loader enum is a loader;
/// an environment entry ([`CURSEFORGE_ENVIRONMENT_NAMES`]) is dropped, since it
/// names a side rather than a release and is read separately, by
/// [`file_declared_sides`]; everything else is left where a Minecraft release would be, which is
/// harmless because compatibility asks whether the *exact* instance release is
/// present rather than what else the list contains.
pub(super) fn split_declared_support(declared: &[String]) -> (Vec<String>, Vec<String>) {
    let mut versions = Vec::new();
    let mut loaders = Vec::new();
    for entry in declared {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if CURSEFORGE_ENVIRONMENT_NAMES
            .iter()
            .any(|name| name.eq_ignore_ascii_case(entry))
        {
            continue;
        }
        if DOCUMENTED_LOADER_NAMES
            .iter()
            .any(|name| name.eq_ignore_ascii_case(entry))
        {
            loaders.push(entry.to_owned());
        } else {
            versions.push(entry.to_owned());
        }
    }
    (versions, loaders)
}

/// The Minecraft releases and loader names a project's latest files declare,
/// each listed once in the order first seen.
///
/// Only the files a search response carries, so this describes what the
/// project recently published rather than everything it ever supported.
pub(super) fn project_declared_support(raw: &CurseForgeProject) -> (Vec<String>, Vec<String>) {
    let mut versions: Vec<String> = Vec::new();
    let mut loaders: Vec<String> = Vec::new();
    for file in &raw.latest_files {
        let (file_versions, file_loaders) = split_declared_support(&file.game_versions);
        for version in file_versions {
            if !versions.contains(&version) {
                versions.push(version);
            }
        }
        for loader in file_loaders {
            if !loaders
                .iter()
                .any(|seen| seen.eq_ignore_ascii_case(&loader))
            {
                loaders.push(loader);
            }
        }
    }
    (versions, loaders)
}

/// How stable a file claims to be.
///
/// The documented `releaseType` values are 1 for a release and 2 for a beta.
/// Everything else (3, which is alpha, an absent tag, and any value a future
/// API adds) is read as the least stable claim, because a build this launcher
/// cannot classify is not one it will present as stable.
const fn channel(release_type: Option<u32>) -> ReleaseChannel {
    match release_type {
        Some(RELEASE_TYPE_RELEASE) => ReleaseChannel::Release,
        Some(RELEASE_TYPE_BETA) => ReleaseChannel::Beta,
        _ => ReleaseChannel::Alpha,
    }
}

/// What one relation means, or `None` for a relation this build does not act
/// on.
///
/// `Tool` has no entry on purpose. A tool is separate software a human might
/// also want, not something the mod needs, and installing one because a file
/// mentioned it would add a mod nobody asked for. An unrecognized value is
/// dropped for the same reason: it never becomes a requirement.
const fn dependency_kind(relation_type: u32) -> Option<ModRelationKind> {
    match relation_type {
        RELATION_REQUIRED_DEPENDENCY => Some(ModRelationKind::Required),
        RELATION_OPTIONAL_DEPENDENCY => Some(ModRelationKind::Optional),
        RELATION_INCOMPATIBLE => Some(ModRelationKind::Incompatible),
        RELATION_EMBEDDED_LIBRARY | RELATION_INCLUDE => Some(ModRelationKind::Embedded),
        _ => None,
    }
}

/// When a file was published.
///
/// Version ordering is by publication date, so an unparsable or absent date
/// becomes the epoch: it sorts oldest, which keeps an undated file from
/// displacing a release that actually is the newest.
pub(super) fn published_at(raw: Option<&str>) -> DateTime<Utc> {
    raw.and_then(|value| DateTime::parse_from_rfc3339(value.trim()).ok())
        .map_or_else(DateTime::<Utc>::default, |value| value.with_timezone(&Utc))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// An identifier the API sent that cannot name anything.
fn invalid_id(field: &str) -> ModPlatformError {
    ModPlatformError::ResponseInvalid {
        provider: ModProvider::CurseForge,
        detail: format!("a file document reported `{field}` as 0, which names nothing"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn project_with_latest_files(game_versions: &[&[&str]]) -> CurseForgeProject {
        let files: Vec<_> = game_versions
            .iter()
            .map(|versions| serde_json::json!({ "gameVersions": versions }))
            .collect();
        let raw = serde_json::json!({
            "id": 1,
            "gameId": 432,
            "name": "X",
            "slug": "x",
            "latestFiles": files,
        });
        serde_json::from_value(raw).expect("parse")
    }

    #[test]
    fn a_file_naming_only_client_makes_the_project_client_only() {
        let raw = project_with_latest_files(&[&["Client", "NeoForge", "26.2"]]);
        let mapped = project(raw);
        assert_eq!(mapped.client_side, SideSupport::Required);
        assert_eq!(mapped.server_side, SideSupport::Unsupported);
    }

    #[test]
    fn a_file_naming_only_server_makes_the_project_server_only() {
        let raw = project_with_latest_files(&[&["NeoForge", "Server", "26.2"]]);
        let mapped = project(raw);
        assert_eq!(mapped.client_side, SideSupport::Unsupported);
        assert_eq!(mapped.server_side, SideSupport::Required);
    }

    #[test]
    fn a_file_naming_neither_side_leaves_the_project_undeclared() {
        let raw = project_with_latest_files(&[&["1.15.2", "Fabric", "1.15.1"]]);
        let mapped = project(raw);
        assert_eq!(mapped.client_side, SideSupport::Unknown);
        assert_eq!(mapped.server_side, SideSupport::Unknown);
    }

    #[test]
    fn a_project_with_no_files_at_all_is_undeclared_rather_than_assumed() {
        let raw = project_with_latest_files(&[]);
        let mapped = project(raw);
        assert_eq!(mapped.client_side, SideSupport::Unknown);
        assert_eq!(mapped.server_side, SideSupport::Unknown);
    }

    #[test]
    fn any_recent_file_naming_a_side_is_enough_to_declare_it_for_the_project() {
        // One old file names neither side, but a newer one names both: the
        // project is read as supporting whatever any of its files declares.
        let raw = project_with_latest_files(&[&["1.16.5", "Fabric"], &["Client", "Server"]]);
        let mapped = project(raw);
        assert_eq!(mapped.client_side, SideSupport::Required);
        assert_eq!(mapped.server_side, SideSupport::Required);
    }

    #[test]
    fn an_environment_entry_never_becomes_a_minecraft_version_or_a_loader() {
        let (versions, loaders) = split_declared_support(&[
            "Client".to_owned(),
            "Server".to_owned(),
            "1.20.1".to_owned(),
        ]);
        assert_eq!(versions, vec!["1.20.1".to_owned()]);
        assert!(loaders.is_empty());
    }
}
