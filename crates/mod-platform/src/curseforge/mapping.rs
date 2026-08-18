//! Reading CurseForge's wire shapes as Ephervex's own model.
//!
//! This is the last place CurseForge's vocabulary appears. Everything returned
//! from here is a [`crate::model`] type, so nothing above this module sees a
//! `classId`, a `relationType`, or a `modLoaderType`.
//!
//! The judgments, in one place, because most of them are refusals:
//!
//! * a project this build cannot confirm is a Mod is never treated as one;
//! * a file with no third-party download address is refused rather than
//!   quietly dropped, because dropping it would offer a *different* release
//!   than the newest one that fits;
//! * a file with no verifiable digest is refused, because verification is not
//!   optional;
//! * a download address is checked against the hosts CurseForge delivers from
//!   before it can name a machine this launcher connects to;
//! * a relation whose type this build does not act on is left out rather than
//!   promoted to a requirement;
//! * CurseForge states nothing about client and server support, so both sides
//!   stay undeclared rather than being filled in from a category or a name.

use chrono::{DateTime, Utc};
use ephervex_download::DomainAllowlist;
use ephervex_model::{CurseForgeFileId, CurseForgeProjectId, ModProvider};

use super::api::{CurseForgeProjectClasses, DOCUMENTED_LOADER_NAMES, supported_hashes};
use super::dto::{
    CurseForgeFileDocument, CurseForgeProject, RELATION_EMBEDDED_LIBRARY, RELATION_INCLUDE,
    RELATION_INCOMPATIBLE, RELATION_OPTIONAL_DEPENDENCY, RELATION_REQUIRED_DEPENDENCY,
    RELEASE_TYPE_BETA, RELEASE_TYPE_RELEASE,
};
use crate::error::{ModPlatformError, ModPlatformResult};
use crate::model::{
    DependencyKind, ModDependency, ModFile, ModProject, ModVersionInfo, ReleaseChannel, SideSupport,
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

/// One confirmed Mods project as the neutral model.
fn project(raw: CurseForgeProject) -> ModProject {
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
        // CurseForge publishes no client/server declaration for a project.
        // Undeclared is the honest answer, and it is what keeps a browsed mod
        // from being pushed onto a linked server on the strength of a guess.
        client_side: SideSupport::Unknown,
        server_side: SideSupport::Unknown,
        icon_url: raw.logo.and_then(|logo| non_empty(logo.url)),
    }
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

    // Null, absent, or blank all mean the same thing: the project's owner
    // turned off third-party distribution. Refused rather than skipped: a
    // skipped file would silently become "install the next one down", which is
    // the substitution this launcher exists not to make.
    let address = document
        .download_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| ModPlatformError::FileDistributionRestricted {
            provider: ModProvider::CurseForge,
            project_id: project.to_string(),
            file_id: file.to_string(),
        })?;

    // Provider data decides which machine the launcher connects to, so it is
    // checked like any other remote address. There is no second address for a
    // CurseForge file, which makes a refusal fatal rather than a fallback.
    let url = hosts
        .validate(address)
        .map_err(|source| ModPlatformError::ResponseInvalid {
            provider: ModProvider::CurseForge,
            detail: source.to_string(),
        })?;

    let hashes = supported_hashes(&document.hashes);
    if hashes.is_empty() {
        return Err(ModPlatformError::HashesMissing {
            version_id: file.to_string(),
        });
    }

    let (game_versions, loaders) = split_declared_support(&document.game_versions);
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
            url: url.to_string(),
            size: document.file_length,
            hashes,
        },
        dependencies: document
            .dependencies
            .iter()
            .filter_map(|relation| {
                Some(ModDependency {
                    kind: dependency_kind(relation.relation_type)?,
                    project_id: Some(CurseForgeProjectId::new(relation.mod_id)?.to_string()),
                    // CurseForge states relations project to project. There is
                    // no file to pin and none is invented: the dependency is
                    // resolved against this instance's own exact target, the
                    // same way the mod that declared it was.
                    version_id: None,
                    file_name: None,
                })
            })
            .collect(),
    })
}

/// Splits one `gameVersions` array into Minecraft releases and loader names.
///
/// CurseForge mixes both, plus values that are neither. Anything matching a
/// name from the API's own documented loader enum is a loader; everything else
/// is left where a Minecraft release would be, which is harmless because
/// compatibility asks whether the *exact* instance release is present rather
/// than what else the list contains.
fn split_declared_support(declared: &[String]) -> (Vec<String>, Vec<String>) {
    let mut versions = Vec::new();
    let mut loaders = Vec::new();
    for entry in declared {
        let entry = entry.trim();
        if entry.is_empty() {
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
const fn dependency_kind(relation_type: u32) -> Option<DependencyKind> {
    match relation_type {
        RELATION_REQUIRED_DEPENDENCY => Some(DependencyKind::Required),
        RELATION_OPTIONAL_DEPENDENCY => Some(DependencyKind::Optional),
        RELATION_INCOMPATIBLE => Some(DependencyKind::Incompatible),
        RELATION_EMBEDDED_LIBRARY | RELATION_INCLUDE => Some(DependencyKind::Embedded),
        _ => None,
    }
}

/// When a file was published.
///
/// Version ordering is by publication date, so an unparsable or absent date
/// becomes the epoch: it sorts oldest, which keeps an undated file from
/// displacing a release that actually is the newest.
fn published_at(raw: Option<&str>) -> DateTime<Utc> {
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
