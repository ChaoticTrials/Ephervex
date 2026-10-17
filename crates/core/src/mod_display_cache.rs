//! What the mods panel can show before anything is asked of a provider.
//!
//! # The problem
//!
//! A host's durable record says which mods it has: a logical path, a jar name,
//! hashes, and a provider project id. It does not say what any of them is
//! called. So the panel paints jar file names and "Version not recorded" and
//! then, once two catalogs have answered, replaces the lot. For a two hundred
//! mod pack that is several seconds of a list that is correct but unreadable,
//! every single time the workspace is opened.
//!
//! Nothing about a mod's name changes often enough to justify asking again
//! before showing anything. So the last answer is kept and the panel opens with
//! it. An answer is kept whenever a provider gives one: when the list is
//! enriched, and when a mod is installed from a catalog.
//!
//! # What it is keyed by
//!
//! Everything kept here belongs to a project or to a release, never to an
//! installation: a title, an author and an icon are facts about a project id; a
//! release's own version string, its Minecraft versions and its loaders are
//! facts about a version id. Two instances holding the same mod therefore
//! describe it identically, and this is a shared store keyed by those ids
//! rather than a copy beside every instance that happens to have the file.
//!
//! Only what one row draws is kept. No description, no identifiers beyond the
//! keys, and nothing a provider response carries that the list does not use.
//! The artwork the icon path points at is the shared artwork cache and is not
//! duplicated here.
//!
//! # On CurseForge
//!
//! CurseForge's third-party API terms ask that data obtained through the API is
//! not saved, which is why [`crate::mods`]'s provider layer routes CurseForge
//! around the metadata cache every other provider uses. Caching a display name
//! for a mod the user has already installed is a deliberate departure from
//! that, decided by the user whose key it is. It is kept to the smallest thing
//! that answers "what is this jar called", and it is refreshed from the
//! provider on every open, so it is a repaint of the last answer rather than a
//! copy of the catalog.
//!
//! # Never authoritative
//!
//! A cache miss, an unreadable file, a version this build does not know: all of
//! them mean the panel opens the way it used to, with file names. Nothing here
//! can change which mods a host has, and nothing reads it to decide anything
//! but what to draw.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ephervex_download::CacheLayout;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Bumped when an entry's shape changes. A file written by a different version
/// is discarded rather than guessed at.
const CACHE_VERSION: u32 = 1;

/// What a provider last said about one project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProjectDisplay {
    pub(crate) title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) author: Option<String>,
    /// The artwork already in the shared cache, when there is any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) icon: Option<PathBuf>,
}

/// What a provider last said about one release.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct VersionDisplay {
    /// The release as a person reads it. Never derived from the file name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) game_versions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) loaders: Vec<String>,
}

/// One row's worth of provider metadata, assembled from what is remembered.
///
/// The shape the panel draws, which is not the shape it is stored in: a row is
/// a project and a release seen together.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ModDisplay {
    pub(crate) title: String,
    pub(crate) version: Option<String>,
    pub(crate) author: Option<String>,
    pub(crate) game_versions: Vec<String>,
    pub(crate) loaders: Vec<String>,
    pub(crate) icon: Option<PathBuf>,
}

/// One provider's remembered answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RememberedDisplay {
    #[serde(default)]
    projects: BTreeMap<String, ProjectDisplay>,
    #[serde(default)]
    versions: BTreeMap<String, VersionDisplay>,
}

impl RememberedDisplay {
    /// The row for one mod, when the project behind it was ever answered for.
    ///
    /// A release that is not remembered is not a miss: the project still names
    /// the mod, and a row with a title and no version reads far better than a
    /// jar file name.
    #[must_use]
    pub(crate) fn compose(&self, project_id: &str, version_id: Option<&str>) -> Option<ModDisplay> {
        let project = self.projects.get(project_id)?;
        let version = version_id.and_then(|id| self.versions.get(id));
        Some(ModDisplay {
            title: project.title.clone(),
            author: project.author.clone(),
            icon: project.icon.clone(),
            version: version.and_then(|version| version.version.clone()),
            game_versions: version
                .map(|it| it.game_versions.clone())
                .unwrap_or_default(),
            loaders: version.map(|it| it.loaders.clone()).unwrap_or_default(),
        })
    }

    /// Records what a provider answered for one project.
    pub(crate) fn remember_project(&mut self, project_id: String, project: ProjectDisplay) {
        self.projects.insert(project_id, project);
    }

    /// Records what a provider answered for one release.
    pub(crate) fn remember_version(&mut self, version_id: String, version: VersionDisplay) {
        self.versions.insert(version_id, version);
    }

    /// The artwork remembered for one project, when there is any.
    #[must_use]
    pub(crate) fn icon(&self, project_id: &str) -> Option<&PathBuf> {
        self.projects.get(project_id)?.icon.as_ref()
    }

    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.projects.is_empty() && self.versions.is_empty()
    }
}

/// A document as it is written, so a file from another build is recognized as
/// one rather than parsed as this build's shape.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    #[serde(flatten)]
    remembered: RememberedDisplay,
}

/// Reads what was last seen for one provider.
///
/// Any failure is an empty answer: this is a repaint, and a launcher that
/// refused to list a host's mods because a cache file was corrupt would be
/// worse than one that lists them under their file names.
#[must_use]
pub(crate) fn read(cache: &CacheLayout, provider: &str) -> RememberedDisplay {
    let Ok(path) = cache.mod_display(provider) else {
        return RememberedDisplay::default();
    };
    let Ok(body) = std::fs::read(&path) else {
        return RememberedDisplay::default();
    };
    match serde_json::from_slice::<CacheFile>(&body) {
        Ok(file) if file.version == CACHE_VERSION => file.remembered,
        Ok(file) => {
            debug!(
                path = %path.display(),
                found = file.version,
                expected = CACHE_VERSION,
                "ignoring a mod display cache from another version"
            );
            RememberedDisplay::default()
        }
        Err(error) => {
            debug!(path = %path.display(), %error, "ignoring an unreadable mod display cache");
            RememberedDisplay::default()
        }
    }
}

/// Records what a provider answered.
///
/// Merged into what is already there rather than replacing it: this describes
/// projects and releases, not one host's list, and a second instance holding
/// different mods from the same provider must not erase the first's. Failure is
/// logged and otherwise ignored; the enrichment the user asked for succeeded
/// either way.
pub(crate) fn write(cache: &CacheLayout, provider: &str, remembered: &RememberedDisplay) {
    if remembered.is_empty() {
        return;
    }
    let Ok(path) = cache.mod_display(provider) else {
        return;
    };
    let mut merged = read(cache, provider);
    merged.projects.extend(
        remembered
            .projects
            .iter()
            .map(|(id, project)| (id.clone(), project.clone())),
    );
    merged.versions.extend(
        remembered
            .versions
            .iter()
            .map(|(id, version)| (id.clone(), version.clone())),
    );

    let file = CacheFile {
        version: CACHE_VERSION,
        remembered: merged,
    };
    // Compact, like every other launcher-owned document.
    let Ok(body) = serde_json::to_vec(&file) else {
        return;
    };
    let Some(directory) = path.parent() else {
        return;
    };
    if let Err(error) = std::fs::create_dir_all(directory) {
        debug!(path = %path.display(), %error, "could not prepare the mod display cache");
        return;
    }
    // Staged and renamed, so an interrupted write leaves the previous answer
    // rather than a truncated one.
    let temporary = path.with_extension("json.tmp");
    if let Err(error) = std::fs::write(&temporary, &body) {
        debug!(path = %temporary.display(), %error, "could not write the mod display cache");
        return;
    }
    if let Err(error) = std::fs::rename(&temporary, &path) {
        debug!(path = %path.display(), %error, "could not publish the mod display cache");
        let _ = std::fs::remove_file(&temporary);
    }
}

/// Removes the per-instance file an earlier build wrote.
///
/// The same metadata is now shared, so this one is dead weight inside every
/// instance that has it. Best-effort and silent: a file that cannot be removed
/// is not read by anything either.
pub(crate) fn forget_instance_local(host_root: &std::path::Path) {
    let path = ephervex_storage::metadata::metadata_dir(host_root).join("mod-display.json");
    if let Err(error) = std::fs::remove_file(&path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        debug!(path = %path.display(), %error, "could not remove a superseded mod display cache");
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn layout(dir: &tempfile::TempDir) -> CacheLayout {
        CacheLayout::new(dir.path())
    }

    fn project(title: &str) -> ProjectDisplay {
        ProjectDisplay {
            title: title.to_owned(),
            author: Some("MelanX".to_owned()),
            icon: Some(PathBuf::from("/cache/artwork/modrinth/abc.png")),
        }
    }

    fn version() -> VersionDisplay {
        VersionDisplay {
            version: Some("1.2.3".to_owned()),
            game_versions: vec!["1.21.1".to_owned()],
            loaders: vec!["neoforge".to_owned()],
        }
    }

    fn remembered() -> RememberedDisplay {
        let mut remembered = RememberedDisplay::default();
        remembered.remember_project("utilitix".to_owned(), project("UtilitiX"));
        remembered.remember_version("v1".to_owned(), version());
        remembered
    }

    #[test]
    fn what_was_written_is_what_comes_back() {
        let dir = tempfile::tempdir().expect("temp");
        let cache = layout(&dir);
        assert!(
            read(&cache, "modrinth").is_empty(),
            "a first open has no cache"
        );

        write(&cache, "modrinth", &remembered());
        assert_eq!(read(&cache, "modrinth"), remembered());
    }

    /// The whole point of the shared store: one answer serves every host that
    /// has the mod.
    #[test]
    fn one_answer_describes_the_mod_wherever_it_is_installed() {
        let dir = tempfile::tempdir().expect("temp");
        let cache = layout(&dir);
        write(&cache, "modrinth", &remembered());

        let row = read(&cache, "modrinth")
            .compose("utilitix", Some("v1"))
            .expect("the project was answered for");
        assert_eq!(row.title, "UtilitiX");
        assert_eq!(row.version.as_deref(), Some("1.2.3"));
        assert_eq!(row.loaders, ["neoforge"]);
    }

    /// A release nobody remembers still leaves a mod with a name.
    #[test]
    fn a_remembered_project_names_a_mod_whose_release_is_unknown() {
        let dir = tempfile::tempdir().expect("temp");
        let cache = layout(&dir);
        write(&cache, "modrinth", &remembered());

        let row = read(&cache, "modrinth")
            .compose("utilitix", None)
            .expect("the project is enough");
        assert_eq!(row.title, "UtilitiX");
        assert_eq!(row.version, None);
        assert!(row.game_versions.is_empty());

        assert!(
            read(&cache, "modrinth")
                .compose("never-seen", None)
                .is_none(),
            "a project nobody answered for has nothing to draw"
        );
    }

    /// Two hosts with different mods from one provider share the file, and the
    /// second must not erase the first.
    #[test]
    fn a_later_answer_is_merged_rather_than_replacing_the_file() {
        let dir = tempfile::tempdir().expect("temp");
        let cache = layout(&dir);
        write(&cache, "modrinth", &remembered());

        let mut second = RememberedDisplay::default();
        second.remember_project("jei".to_owned(), project("Just Enough Items"));
        write(&cache, "modrinth", &second);

        let stored = read(&cache, "modrinth");
        assert!(stored.compose("utilitix", None).is_some(), "still there");
        assert!(
            stored.compose("jei", None).is_some(),
            "and so is the new one"
        );
    }

    #[test]
    fn each_provider_keeps_its_own_answers() {
        let dir = tempfile::tempdir().expect("temp");
        let cache = layout(&dir);
        write(&cache, "modrinth", &remembered());

        assert!(
            read(&cache, "curseforge").is_empty(),
            "one provider's ids mean nothing to another"
        );
    }

    #[test]
    fn the_document_is_compact_like_every_other_launcher_owned_file() {
        let dir = tempfile::tempdir().expect("temp");
        let cache = layout(&dir);
        write(&cache, "modrinth", &remembered());

        let body = std::fs::read_to_string(cache.mod_display("modrinth").expect("path"))
            .expect("the cache was written");
        assert!(!body.contains('\n'), "{body}");
    }

    #[test]
    fn a_cache_that_cannot_be_understood_is_a_miss_rather_than_a_failure() {
        let dir = tempfile::tempdir().expect("temp");
        let cache = layout(&dir);
        let path = cache.mod_display("modrinth").expect("path");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");

        std::fs::write(&path, b"not json at all").expect("write");
        assert!(read(&cache, "modrinth").is_empty());

        // A file from a build that shaped its entries differently.
        std::fs::write(&path, br#"{"version":99,"projects":{"a":{"title":"x"}}}"#).expect("write");
        assert!(read(&cache, "modrinth").is_empty());
    }

    #[test]
    fn a_superseded_per_instance_cache_is_removed() {
        let dir = tempfile::tempdir().expect("temp");
        let stale = ephervex_storage::metadata::metadata_dir(dir.path()).join("mod-display.json");
        std::fs::create_dir_all(stale.parent().expect("parent")).expect("dir");
        std::fs::write(&stale, b"{}").expect("write");

        forget_instance_local(dir.path());
        assert!(!stale.exists());
        // Removing one that is not there is not a failure.
        forget_instance_local(dir.path());
    }
}
