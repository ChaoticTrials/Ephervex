//! What the mods panel can show before anything is asked of a provider.
//!
//! # The problem
//!
//! An instance's durable record says which mods it has: a logical path, a jar
//! name, hashes, and a provider project id. It does not say what any of them is
//! called. So the panel paints jar file names and "Version not recorded" and
//! then, once two catalogs have answered, replaces the lot. For a two hundred
//! mod pack that is several seconds of a list that is correct but unreadable,
//! every single time the workspace is opened.
//!
//! Nothing about a mod's name changes often enough to justify asking again
//! before showing anything. So the last answer is kept beside the instance it
//! describes, and the panel opens with it.
//!
//! # What is kept, and what is not
//!
//! Only what one row draws: the title, the installed version as a string, the
//! Minecraft versions and loaders the row summarizes, the author line, and the
//! path of the artwork that was already cached. No description, no identifiers
//! beyond the logical path the entry is keyed by, and nothing a provider
//! response carries that the list does not use.
//!
//! This is a per-instance file rather than a shared store because it is a view
//! of one instance's list, keyed by that instance's own logical paths. The
//! artwork it points at is the shared cache and is not duplicated here.
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
//! can change which mods an instance has, and nothing reads it to decide
//! anything but what to draw.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::debug;

/// `.launcher/mod-display.json`.
const CACHE_FILE: &str = "mod-display.json";

/// Bumped when an entry's shape changes. A file written by a different version
/// is discarded rather than guessed at.
const CACHE_VERSION: u32 = 1;

/// One row's worth of provider metadata, as last seen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModDisplay {
    pub title: String,
    /// The installed release as a person reads it, when the exact release was
    /// known. Never derived from the file name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub game_versions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loaders: Vec<String>,
    /// The artwork already in the shared cache, when there is any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<PathBuf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    /// Keyed by the entry's logical path, which is what identifies a row within
    /// one instance and survives a mod being renamed on its provider.
    entries: BTreeMap<String, ModDisplay>,
}

/// Reads what was last seen for one instance, by logical path.
///
/// Any failure is an empty map: this is a repaint, and a launcher that refused
/// to list an instance's mods because a cache file was corrupt would be worse
/// than one that lists them under their file names.
#[must_use]
pub(crate) fn read(instance_root: &Path) -> BTreeMap<String, ModDisplay> {
    let path = cache_path(instance_root);
    let Ok(body) = std::fs::read(&path) else {
        return BTreeMap::new();
    };
    match serde_json::from_slice::<CacheFile>(&body) {
        Ok(cache) if cache.version == CACHE_VERSION => cache.entries,
        Ok(cache) => {
            debug!(
                path = %path.display(),
                found = cache.version,
                expected = CACHE_VERSION,
                "ignoring a mod display cache from another version"
            );
            BTreeMap::new()
        }
        Err(error) => {
            debug!(path = %path.display(), %error, "ignoring an unreadable mod display cache");
            BTreeMap::new()
        }
    }
}

/// Records what a provider answered, replacing whatever was there.
///
/// Written whole rather than merged: the list it describes is the whole list,
/// and an entry that is no longer in it belongs to a mod the instance no longer
/// has. Failure is logged and otherwise ignored; the enrichment the user asked
/// for succeeded either way.
pub(crate) fn write(instance_root: &Path, entries: BTreeMap<String, ModDisplay>) {
    let path = cache_path(instance_root);
    let cache = CacheFile {
        version: CACHE_VERSION,
        entries,
    };
    // Compact, like every other launcher-owned document under `.launcher`.
    let Ok(body) = serde_json::to_vec(&cache) else {
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

fn cache_path(instance_root: &Path) -> PathBuf {
    ephervex_storage::metadata::metadata_dir(instance_root).join(CACHE_FILE)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn entry(title: &str) -> ModDisplay {
        ModDisplay {
            title: title.to_owned(),
            version: Some("1.2.3".to_owned()),
            author: Some("MelanX".to_owned()),
            game_versions: vec!["1.21.1".to_owned()],
            loaders: vec!["neoforge".to_owned()],
            icon: Some(PathBuf::from("/cache/artwork/modrinth/abc.png")),
        }
    }

    #[test]
    fn what_was_written_is_what_comes_back() {
        let root = tempfile::tempdir().expect("temp");
        assert!(read(root.path()).is_empty(), "a first open has no cache");

        let mut entries = BTreeMap::new();
        entries.insert("mods/utilitix.jar".to_owned(), entry("UtilitiX"));
        write(root.path(), entries.clone());

        assert_eq!(read(root.path()), entries);
    }

    #[test]
    fn the_document_is_compact_like_every_other_launcher_owned_file() {
        let root = tempfile::tempdir().expect("temp");
        let mut entries = BTreeMap::new();
        entries.insert("mods/utilitix.jar".to_owned(), entry("UtilitiX"));
        write(root.path(), entries);

        let body = std::fs::read_to_string(cache_path(root.path())).expect("the cache was written");
        assert!(!body.contains('\n'), "{body}");
    }

    #[test]
    fn a_cache_that_cannot_be_understood_is_a_miss_rather_than_a_failure() {
        let root = tempfile::tempdir().expect("temp");
        let path = cache_path(root.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");

        std::fs::write(&path, b"not json at all").expect("write");
        assert!(read(root.path()).is_empty());

        // A file from a build that shaped its entries differently.
        std::fs::write(&path, br#"{"version":99,"entries":{"a":{"title":"x"}}}"#).expect("write");
        assert!(read(root.path()).is_empty());
    }

    #[test]
    fn a_later_answer_replaces_the_earlier_one_whole() {
        let root = tempfile::tempdir().expect("temp");
        let mut first = BTreeMap::new();
        first.insert("mods/a.jar".to_owned(), entry("A"));
        first.insert("mods/gone.jar".to_owned(), entry("Removed"));
        write(root.path(), first);

        let mut second = BTreeMap::new();
        second.insert("mods/a.jar".to_owned(), entry("A"));
        write(root.path(), second.clone());

        assert_eq!(read(root.path()), second, "a removed mod leaves no entry");
    }
}
