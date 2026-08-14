//! Where an instance keeps what its pack said (ADR 0002).
//!
//! ```text
//! <instance>/.launcher/source/
//! ├── recipe.json              normalized, provider-neutral
//! └── modrinth/                verbatim provider state
//!     ├── modrinth.index.json
//!     ├── overrides/
//!     └── server-overrides/
//! ```
//!
//! # Why both halves exist
//!
//! `recipe.json` is what the launcher reads: it is the same shape whichever
//! provider produced it, so "Create Server", the instance UI, and the installer
//! are written once. The provider directory is what the launcher *keeps*: the
//! original manifest byte for byte, because a normalized model is a lossy view
//! by construction and a bug report six months from now may need the original.
//!
//! # Which override layers are retained, and why not all of them
//!
//! The common and server layers are copied here, because a server created later
//! needs them and the `.mrpack` will be long gone. The client layer is not:
//! It is already present in the game
//! directory, and a modpack's overrides are the bulk of its bytes — duplicating
//! them would double the size of every imported instance to preserve something
//! nothing reads. The recipe records the decision per layer
//! ([`crate::recipe::OverrideLayer::retained_at`]) rather than leaving a caller
//! to guess, so a plan can say out loud that a layer is unavailable.

use std::path::{Path, PathBuf};

use ephervex_model::ImportSourceKind;

use crate::error::{PackError, PackResult};
use crate::recipe::{PackRecipe, RECIPE_VERSION};

/// The normalized recipe's file name inside `.launcher/source/`.
pub const RECIPE_FILE: &str = "recipe.json";

/// The directory name one provider's retained state lives under.
///
/// Lowercase and stable: it is part of an on-disk layout users inspect and
/// attach to bug reports, so it is not derived from a `Display` implementation
/// that exists to be read by humans.
#[must_use]
pub const fn provider_directory(kind: ImportSourceKind) -> &'static str {
    match kind {
        ImportSourceKind::ModrinthPack => "modrinth",
        ImportSourceKind::CurseForgePack => "curseforge",
        ImportSourceKind::PrismExport => "prism",
        ImportSourceKind::Manual => "manual",
    }
}

/// One instance's retained source directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStore {
    dir: PathBuf,
}

impl SourceStore {
    /// Wraps `<instance>/.launcher/source/`, which `ephervex-storage` creates
    /// when the instance is created.
    #[must_use]
    pub fn new(source_dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: source_dir.into(),
        }
    }

    /// The directory itself.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `<source>/recipe.json`.
    #[must_use]
    pub fn recipe_path(&self) -> PathBuf {
        self.dir.join(RECIPE_FILE)
    }

    /// Whether this instance has a retained recipe at all.
    ///
    /// False for an instance created by hand, which is not a failure: a retained
    /// recipe describes an *import*.
    #[must_use]
    pub fn has_recipe(&self) -> bool {
        self.recipe_path().is_file()
    }

    /// `<source>/<provider>/`.
    #[must_use]
    pub fn provider_dir(&self, kind: ImportSourceKind) -> PathBuf {
        self.dir.join(provider_directory(kind))
    }

    /// Turns a path recorded in the recipe into an absolute one.
    ///
    /// Recipe paths are stored relative to this directory so that copying an
    /// instance — or copying its recipe into a server — does not invalidate
    /// them.
    #[must_use]
    pub fn resolve(&self, relative: &Path) -> PathBuf {
        self.dir.join(relative)
    }

    /// Writes the normalized recipe.
    ///
    /// Pretty-printed, because these files end up pasted into bug reports, and
    /// written through a temporary file so an interrupted write cannot turn a
    /// recoverable instance into an unreadable one — the same rename dance
    /// `ephervex-storage` uses for instance metadata.
    pub fn write_recipe(&self, recipe: &PackRecipe) -> PackResult<PathBuf> {
        Self::ensure_dir(&self.dir)?;
        let path = self.recipe_path();
        let text = serde_json::to_string_pretty(recipe).map_err(|error| PackError::Unwritable {
            path: path.clone(),
            source: std::io::Error::other(error),
        })?;

        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text).map_err(|source| PackError::Unwritable {
            path: temporary.clone(),
            source,
        })?;
        std::fs::rename(&temporary, &path).map_err(|source| PackError::Unwritable {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    /// Reads the normalized recipe back.
    ///
    /// The version is checked before the payload is parsed: a recipe written by
    /// a newer build may have moved fields this one would otherwise misread as
    /// absent, and silently importing half a server recipe is worse than saying
    /// so.
    pub fn read_recipe(&self) -> PackResult<PackRecipe> {
        let path = self.recipe_path();
        let text =
            std::fs::read_to_string(&path).map_err(|source| PackError::RecipeUnreadable {
                path: path.clone(),
                source,
            })?;

        let envelope: serde_json::Value =
            serde_json::from_str(&text).map_err(|source| PackError::RecipeInvalid {
                path: path.clone(),
                source,
            })?;
        let found = envelope
            .get("recipe_version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(RECIPE_VERSION);
        if found > RECIPE_VERSION {
            return Err(PackError::RecipeUnsupportedVersion {
                path,
                found,
                supported: RECIPE_VERSION,
            });
        }

        let mut recipe: PackRecipe = serde_json::from_value(envelope)
            .map_err(|source| PackError::RecipeInvalid { path, source })?;
        crate::import::reconcile_identities(&mut recipe);
        Ok(recipe)
    }

    /// Keeps a provider's manifest byte for byte, returning its path relative
    /// to this directory.
    ///
    /// `name` is a constant from the provider's own format definition, never a
    /// name taken from the archive, so it cannot choose a destination
    pub fn retain_manifest(
        &self,
        kind: ImportSourceKind,
        name: &'static str,
        bytes: &[u8],
    ) -> PackResult<PathBuf> {
        let directory = self.provider_dir(kind);
        Self::ensure_dir(&directory)?;
        let path = directory.join(name);
        std::fs::write(&path, bytes).map_err(|source| PackError::Unwritable {
            path: path.clone(),
            source,
        })?;
        Ok(PathBuf::from(provider_directory(kind)).join(name))
    }

    /// The directory one retained override layer lives in, and its path
    /// relative to this store.
    ///
    /// `prefix` comes from the provider's format definition (`overrides`,
    /// `server-overrides`), and is reduced to its last path component so a
    /// provider cannot spell it as something that climbs out.
    #[must_use]
    pub fn layer_dir(&self, kind: ImportSourceKind, prefix: &str) -> (PathBuf, PathBuf) {
        let name = prefix
            .trim_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(prefix)
            .replace(['\\', ':'], "-");
        let relative = PathBuf::from(provider_directory(kind)).join(name);
        (self.dir.join(&relative), relative)
    }

    fn ensure_dir(path: &Path) -> PackResult<()> {
        std::fs::create_dir_all(path).map_err(|source| PackError::Unwritable {
            path: path.to_path_buf(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::{LoaderKind, LoaderSelection};

    use super::*;
    use crate::recipe::ProviderRecipeMetadata;

    fn recipe() -> PackRecipe {
        PackRecipe {
            recipe_version: RECIPE_VERSION,
            source_kind: ImportSourceKind::ModrinthPack,
            original_name: "Test Pack".to_owned(),
            original_version: None,
            summary: None,
            archive_name: Some("test.mrpack".to_owned()),
            minecraft_version: "1.20.1".to_owned(),
            loader: LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
            files: Vec::new(),
            override_layers: Vec::new(),
            provider_metadata: ProviderRecipeMetadata::new(ImportSourceKind::ModrinthPack, None),
        }
    }

    #[test]
    fn a_recipe_round_trips_through_the_store() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SourceStore::new(dir.path().join("source"));
        assert!(!store.has_recipe());

        store.write_recipe(&recipe()).expect("write");
        assert!(store.has_recipe());
        assert_eq!(store.read_recipe().expect("read"), recipe());

        // No half-written temporary file left behind.
        assert!(!store.dir().join("recipe.json.tmp").exists());
    }

    #[test]
    fn a_manifest_is_retained_verbatim_under_its_provider() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SourceStore::new(dir.path().join("source"));
        let relative = store
            .retain_manifest(
                ImportSourceKind::ModrinthPack,
                "modrinth.index.json",
                b"{\"formatVersion\":1}",
            )
            .expect("retain");

        assert_eq!(relative, PathBuf::from("modrinth/modrinth.index.json"));
        assert_eq!(
            std::fs::read_to_string(store.resolve(&relative)).expect("read"),
            "{\"formatVersion\":1}"
        );
    }

    #[test]
    fn a_recipe_from_a_newer_build_is_refused_rather_than_misread() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SourceStore::new(dir.path().join("source"));
        store.write_recipe(&recipe()).expect("write");

        let path = store.recipe_path();
        let text = std::fs::read_to_string(&path).expect("read").replace(
            &format!("\"recipe_version\": {RECIPE_VERSION}"),
            "\"recipe_version\": 99",
        );
        std::fs::write(&path, text).expect("write");

        let error = store.read_recipe().expect_err("must refuse");
        assert!(matches!(
            error,
            PackError::RecipeUnsupportedVersion {
                found: 99,
                supported: RECIPE_VERSION,
                ..
            }
        ));
    }

    #[test]
    fn a_recipe_written_before_managed_mods_existed_still_reads() {
        // Version 1 recipes are on disk in every instance imported by an
        // earlier build. Refusing them, or misreading their files, would strand
        // instances that are working perfectly well.
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SourceStore::new(dir.path().join("source"));
        std::fs::create_dir_all(store.dir()).expect("mkdir");
        std::fs::write(
            store.recipe_path(),
            r#"{
              "recipe_version": 1,
              "source_kind": "modrinth_pack",
              "original_name": "Older Import",
              "minecraft_version": "1.20.1",
              "loader": {"kind": "neo_forge", "version": "47.1.106"},
              "files": [{
                "logical_path": "mods/a.jar",
                "hashes": [{"algorithm": "sha1", "digest": "da39a3ee5e6b4b0d3255bfef95601890afd80709"}],
                "downloads": ["https://cdn.modrinth.com/data/x/a.jar"],
                "client": "required",
                "server": "required"
              }],
              "provider_metadata": {"kind": "modrinth_pack"}
            }"#,
        )
        .expect("write");

        let recipe = store.read_recipe().expect("a version 1 recipe still reads");
        assert_eq!(recipe.recipe_version, 1);
        assert_eq!(recipe.files.len(), 1);
        assert_eq!(
            recipe.files[0].origin,
            ephervex_model::ModOrigin::PackDeclared,
            "a file written before origins existed is what it always was: the pack's"
        );
        assert!(
            recipe.managed_mods().is_empty(),
            "nothing in an old recipe is retroactively a managed mod"
        );
    }

    #[test]
    fn a_layer_prefix_cannot_choose_a_destination_outside_the_provider_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SourceStore::new(dir.path().join("source"));
        let (absolute, relative) =
            store.layer_dir(ImportSourceKind::ModrinthPack, "../../escape/overrides");
        assert_eq!(relative, PathBuf::from("modrinth/overrides"));
        assert!(absolute.starts_with(store.dir()));
    }

    #[test]
    fn each_provider_keeps_its_state_in_its_own_directory() {
        assert_eq!(
            provider_directory(ImportSourceKind::ModrinthPack),
            "modrinth"
        );
        assert_eq!(
            provider_directory(ImportSourceKind::CurseForgePack),
            "curseforge"
        );
        assert_eq!(provider_directory(ImportSourceKind::PrismExport), "prism");
    }

    #[test]
    fn a_corrupt_recipe_names_the_file_rather_than_panicking() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SourceStore::new(dir.path().join("source"));
        std::fs::create_dir_all(store.dir()).expect("mkdir");
        std::fs::write(store.recipe_path(), "{not json").expect("write");

        let error = store.read_recipe().expect_err("must fail");
        assert!(matches!(error, PackError::RecipeInvalid { .. }));
        assert_eq!(error.failing_path(), Some(store.recipe_path()));
    }
}
