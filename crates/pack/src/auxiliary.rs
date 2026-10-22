//! Resource packs, shader packs, and data packs a pack places in the game
//! directory.
//!
//! These are not mods and never enter the mod list. They are recognized by the
//! directory they sit in, because that is the only thing every source format
//! agrees on: a Modrinth index and an override tree name a path, and a
//! CurseForge project is routed to the directory of its confirmed class.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::managed::{RESOURCEPACKS_DIR, SHADERPACKS_DIR};
use crate::recipe::{OptionalSelection, OverrideScope, PackFileRecipe, PackRecipe, PackSide};

/// The directory at the game root holding data packs that belong to no world.
pub const DATAPACKS_DIR: &str = "datapacks";
/// The directory holding a client's worlds.
pub const SAVES_DIR: &str = "saves";
/// A dedicated server's default world folder.
pub const SERVER_WORLD_DIR: &str = "world";

/// What kind of pack content a path holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuxiliaryKind {
    ResourcePack,
    ShaderPack,
    DataPack,
}

impl AuxiliaryKind {
    /// A stable identifier for `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResourcePack => "resource-pack",
            Self::ShaderPack => "shader-pack",
            Self::DataPack => "data-pack",
        }
    }
}

/// The world a data pack belongs to, as its path says.
///
/// Never inferred from anything but the path, and never moved: a data pack
/// stays in the world folder the pack placed it in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DataPackOwner {
    /// `saves/<name>/datapacks/`, a client world.
    SavedWorld(String),
    /// `world/datapacks/`, a dedicated server's default world folder.
    ServerWorld,
    /// `datapacks/` at the game root. No world owns it, so Minecraft does not
    /// apply it by itself.
    NoWorld,
}

impl DataPackOwner {
    /// A stable identifier for `--json` output.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::SavedWorld(_) => "saved-world",
            Self::ServerWorld => "server-world",
            Self::NoWorld => "no-world",
        }
    }

    /// The world's name, for a client world.
    #[must_use]
    pub fn world_name(&self) -> Option<&str> {
        match self {
            Self::SavedWorld(name) => Some(name),
            Self::ServerWorld | Self::NoWorld => None,
        }
    }
}

/// Where a path puts auxiliary content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuxiliaryPlacement {
    pub kind: AuxiliaryKind,
    /// Set for a data pack, `None` otherwise.
    pub owner: Option<DataPackOwner>,
    /// The pack itself relative to the game directory: the file, or the
    /// directory a multi-file pack sits in.
    pub entry: PathBuf,
}

/// Where `path` puts auxiliary content, judged by location alone.
#[must_use]
pub fn classify(path: &Path) -> Option<AuxiliaryPlacement> {
    let segments: Vec<&str> = path
        .components()
        .map(|component| match component {
            Component::Normal(segment) => segment.to_str(),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let (kind, owner, depth) = match segments.as_slice() {
        [RESOURCEPACKS_DIR, _, ..] => (AuxiliaryKind::ResourcePack, None, 2),
        [SHADERPACKS_DIR, _, ..] => (AuxiliaryKind::ShaderPack, None, 2),
        [SAVES_DIR, world, DATAPACKS_DIR, _, ..] => (
            AuxiliaryKind::DataPack,
            Some(DataPackOwner::SavedWorld((*world).to_owned())),
            4,
        ),
        [SERVER_WORLD_DIR, DATAPACKS_DIR, _, ..] => {
            (AuxiliaryKind::DataPack, Some(DataPackOwner::ServerWorld), 3)
        }
        [DATAPACKS_DIR, _, ..] => (AuxiliaryKind::DataPack, Some(DataPackOwner::NoWorld), 2),
        _ => return None,
    };
    Some(AuxiliaryPlacement {
        kind,
        owner,
        entry: segments.iter().take(depth).collect(),
    })
}

/// [`classify`] for a file a pack shipped inside an override tree.
///
/// A single file directly in one of these directories counts only when it is a
/// `.zip`, so a loader's options file or a README beside the packs is not
/// reported as one. A file inside a directory counts as part of that pack.
#[must_use]
pub fn classify_shipped(path: &Path) -> Option<AuxiliaryPlacement> {
    let placement = classify(path)?;
    let inside_directory_pack = path.components().count() > placement.entry.components().count();
    let is_archive = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"));
    (inside_directory_pack || is_archive).then_some(placement)
}

/// How an auxiliary entry came to be part of the instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuxiliarySource {
    /// The pack named the file, so the recipe records its identity, digests,
    /// and download addresses.
    Declared(Box<PackFileRecipe>),
    /// The pack carried the bytes in an override layer.
    Shipped(OverrideScope),
}

/// One resource pack, shader pack, or data pack a pack placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuxiliaryContent {
    pub placement: AuxiliaryPlacement,
    pub source: AuxiliarySource,
}

impl AuxiliaryContent {
    /// Whether the recipe installs this entry on `side` without further input.
    ///
    /// A shipped entry installs wherever its layer applies, which is why it was
    /// listed for that side at all.
    #[must_use]
    pub fn installs_on(&self, side: PackSide) -> bool {
        match &self.source {
            AuxiliarySource::Declared(file) => {
                file.is_installed_on(side, &OptionalSelection::none())
            }
            AuxiliarySource::Shipped(_) => true,
        }
    }
}

impl PackRecipe {
    /// The auxiliary content this recipe puts on one side, in display order.
    ///
    /// Declared files the side excludes are left out. A shipped entry comes from
    /// the layers that apply to the side, the later layer naming the winner when
    /// two place the same path.
    #[must_use]
    pub fn auxiliary_content(&self, side: PackSide) -> Vec<AuxiliaryContent> {
        let mut content: Vec<AuxiliaryContent> = self
            .files
            .iter()
            .filter(|file| !file.is_excluded_from(side))
            .filter_map(|file| {
                classify(&file.logical_path).map(|placement| AuxiliaryContent {
                    placement,
                    source: AuxiliarySource::Declared(Box::new(file.clone())),
                })
            })
            .collect();
        for layer in self.layers_for(side) {
            for entry in &layer.auxiliary_entries {
                let Some(placement) = classify(entry) else {
                    continue;
                };
                content.retain(|existing| existing.placement.entry != placement.entry);
                content.push(AuxiliaryContent {
                    placement,
                    source: AuxiliarySource::Shipped(layer.scope),
                });
            }
        }
        content.sort_by(|left, right| {
            (
                left.placement.kind,
                &left.placement.owner,
                &left.placement.entry,
            )
                .cmp(&(
                    right.placement.kind,
                    &right.placement.owner,
                    &right.placement.entry,
                ))
        });
        content
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn placement(path: &str) -> Option<AuxiliaryPlacement> {
        classify(Path::new(path))
    }

    #[test]
    fn a_resource_pack_file_and_a_resource_pack_folder_are_each_one_entry() {
        let file = placement("resourcepacks/Faithful.zip").unwrap();
        assert_eq!(file.kind, AuxiliaryKind::ResourcePack);
        assert_eq!(file.entry, Path::new("resourcepacks/Faithful.zip"));

        let folder = placement("resourcepacks/Faithful/assets/minecraft/lang/en_us.json").unwrap();
        assert_eq!(folder.entry, Path::new("resourcepacks/Faithful"));
    }

    #[test]
    fn a_shader_pack_is_recognized_under_shaderpacks() {
        let shader = placement("shaderpacks/Complementary.zip").unwrap();
        assert_eq!(shader.kind, AuxiliaryKind::ShaderPack);
        assert_eq!(shader.owner, None);
    }

    #[test]
    fn a_saved_worlds_data_pack_names_that_world() {
        let pack = placement("saves/New World/datapacks/tweaks.zip").unwrap();
        assert_eq!(pack.kind, AuxiliaryKind::DataPack);
        assert_eq!(
            pack.owner,
            Some(DataPackOwner::SavedWorld("New World".to_owned()))
        );
        assert_eq!(
            pack.entry,
            Path::new("saves/New World/datapacks/tweaks.zip")
        );
    }

    #[test]
    fn a_servers_world_folder_data_pack_is_owned_by_the_server_world() {
        let pack = placement("world/datapacks/tweaks/data/x/recipes/a.json").unwrap();
        assert_eq!(pack.owner, Some(DataPackOwner::ServerWorld));
        assert_eq!(pack.entry, Path::new("world/datapacks/tweaks"));
    }

    #[test]
    fn a_game_root_data_pack_belongs_to_no_world() {
        let pack = placement("datapacks/tweaks.zip").unwrap();
        assert_eq!(pack.owner, Some(DataPackOwner::NoWorld));
    }

    #[test]
    fn paths_elsewhere_and_bare_directories_are_not_auxiliary_content() {
        for path in [
            "mods/a.jar",
            "config/resourcepacks/x.zip",
            "saves/World/level.dat",
            "saves/World/datapacks",
            "resourcepacks",
            "other/datapacks/x.zip",
        ] {
            assert_eq!(placement(path), None, "{path}");
        }
    }

    #[test]
    fn a_shipped_options_file_beside_the_packs_is_not_a_pack() {
        assert_eq!(
            classify_shipped(Path::new("shaderpacks/Pack.zip.txt")),
            None
        );
        assert_eq!(classify_shipped(Path::new("resourcepacks/README.md")), None);
        assert!(classify_shipped(Path::new("shaderpacks/Pack.ZIP")).is_some());
        assert!(classify_shipped(Path::new("shaderpacks/Pack/shaders/final.fsh")).is_some());
    }
}
