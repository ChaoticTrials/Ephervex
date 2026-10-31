//! The resource packs, shader packs, and data packs a host's pack placed.
//!
//! Read from the retained recipe and the game directory alone, so it works
//! offline and writes nothing. These are never mods: the mod list does not
//! show them and nothing here routes through it.

use std::path::{Path, PathBuf};

use ephervex_model::ModProvider;
use ephervex_pack::{
    AuxiliaryContent, AuxiliaryKind, AuxiliarySource, DataPackOwner, OverrideScope, PackSide,
    SideRequirement,
};
use tracing::instrument;

use crate::app::App;
use crate::error::CoreResult;
use crate::mods::{InstalledHost, InstancePackSource};

/// How the pack came to place an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackContentOrigin {
    /// The pack named the file. Its identity and digests are in the recipe.
    Declared {
        provider: Option<ModProvider>,
        project_id: Option<String>,
        version_id: Option<String>,
        /// Whether the recipe records a digest to check the file against.
        verifiable: bool,
        requirement: SideRequirement,
    },
    /// The pack carried the bytes in an override layer.
    Shipped { layer: OverrideScope },
}

/// Whether an entry is in the game directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackContentPresence {
    Present,
    /// The recipe installs it on this host and the game directory lacks it.
    Absent,
    /// The pack offered it as optional and it was not selected.
    NotSelected,
}

impl PackContentPresence {
    /// A stable identifier for `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
            Self::NotSelected => "not-selected",
        }
    }
}

/// One resource pack, shader pack, or data pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackContentEntry {
    pub kind: AuxiliaryKind,
    /// The world a data pack belongs to. `None` for every other kind.
    pub owner: Option<DataPackOwner>,
    /// The pack's file or directory relative to the game directory.
    pub logical_path: PathBuf,
    pub origin: PackContentOrigin,
    pub presence: PackContentPresence,
}

impl PackContentEntry {
    /// The pack's own file or directory name.
    #[must_use]
    pub fn name(&self) -> String {
        self.logical_path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    }
}

/// Everything one host's pack placed, grouped by nothing: callers filter by
/// [`PackContentEntry::kind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackContentSnapshot {
    pub host: InstalledHost,
    /// The import this host came from, when it came from one.
    pub source: Option<InstancePackSource>,
    pub entries: Vec<PackContentEntry>,
}

impl App {
    /// The resource packs, shader packs, and data packs one host's pack placed.
    ///
    /// Empty for a host with no retained recipe, and for one imported before
    /// override layers recorded their packs: re-importing records them.
    #[instrument(skip(self), fields(host = %host.id()))]
    pub fn host_pack_content(&self, host: InstalledHost) -> CoreResult<PackContentSnapshot> {
        let layout = self.host_layout(host)?;
        let side = host.side();
        let entries = layout
            .recipe
            .as_ref()
            .map(|recipe| recipe.auxiliary_content(side))
            .unwrap_or_default()
            .into_iter()
            .map(|content| entry_for(content, side, &layout.game_dir))
            .collect();
        Ok(PackContentSnapshot {
            host,
            source: layout.recipe.as_ref().map(InstancePackSource::from_recipe),
            entries,
        })
    }
}

fn entry_for(content: AuxiliaryContent, side: PackSide, game_dir: &Path) -> PackContentEntry {
    let installs = content.installs_on(side);
    let AuxiliaryContent { placement, source } = content;
    let origin = match source {
        AuxiliarySource::Declared(file) => PackContentOrigin::Declared {
            provider: file
                .identity
                .as_ref()
                .and_then(|identity| identity.provider),
            project_id: file
                .identity
                .as_ref()
                .and_then(|identity| identity.project_id.clone()),
            version_id: file
                .identity
                .as_ref()
                .and_then(|identity| identity.version_id.clone()),
            verifiable: !file.hashes.is_empty(),
            requirement: file.requirement(side),
        },
        AuxiliarySource::Shipped(layer) => PackContentOrigin::Shipped { layer },
    };
    let present = std::fs::symlink_metadata(game_dir.join(&placement.entry)).is_ok();
    let presence = match (installs, present) {
        (false, false) => PackContentPresence::NotSelected,
        (_, true) => PackContentPresence::Present,
        (true, false) => PackContentPresence::Absent,
    };
    PackContentEntry {
        kind: placement.kind,
        owner: placement.owner,
        logical_path: placement.entry,
        origin,
        presence,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::io::Write as _;

    use ephervex_download::OperationReporter;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::import::ImportRequest;

    const INDEX: &[u8] = br#"{
        "formatVersion": 1,
        "game": "minecraft",
        "versionId": "3.2.1",
        "name": "Reproduction Pack",
        "files": [],
        "dependencies": {"minecraft": "1.20.1", "neoforge": "47.1.106"}
    }"#;

    fn pack_with_overrides(path: &Path) {
        let file = std::fs::File::create(path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("modrinth.index.json", options)
            .expect("start index");
        writer.write_all(INDEX).expect("write index");
        for name in [
            "overrides/resourcepacks/Faithful.zip",
            "overrides/shaderpacks/Complementary.zip",
            "overrides/saves/Repro/datapacks/tweaks.zip",
            "overrides/datapacks/loose.zip",
            "server-overrides/world/datapacks/server.zip",
        ] {
            writer.start_file(name, options).expect("start entry");
            writer.write_all(b"pack").expect("write entry");
        }
        writer.finish().expect("finish archive");
    }

    async fn imported_instance(base: &Path) -> (App, ephervex_model::InstanceId) {
        let archive_dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_dir.path().join("pack.mrpack");
        pack_with_overrides(&archive);
        let app = App::open_at(ephervex_storage::Paths::rooted_at(base)).expect("app opens");
        let imported = app
            .import_pack(
                &ImportRequest::new(&archive),
                &OperationReporter::silent("test-import"),
                &CancellationToken::new(),
            )
            .await
            .expect("import the pack");
        (app, imported.record.id)
    }

    #[tokio::test]
    async fn an_imported_instance_lists_the_packs_it_received_with_their_owning_world() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance) = imported_instance(base.path()).await;

        let snapshot = app
            .host_pack_content(InstalledHost::Instance(instance))
            .expect("snapshot");

        let listed: Vec<_> = snapshot
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.logical_path.to_string_lossy().into_owned(),
                    entry.kind,
                    entry.owner.clone(),
                    entry.presence,
                )
            })
            .collect();
        assert_eq!(
            listed,
            vec![
                (
                    "resourcepacks/Faithful.zip".to_owned(),
                    AuxiliaryKind::ResourcePack,
                    None,
                    PackContentPresence::Present
                ),
                (
                    "shaderpacks/Complementary.zip".to_owned(),
                    AuxiliaryKind::ShaderPack,
                    None,
                    PackContentPresence::Present
                ),
                (
                    "saves/Repro/datapacks/tweaks.zip".to_owned(),
                    AuxiliaryKind::DataPack,
                    Some(DataPackOwner::SavedWorld("Repro".to_owned())),
                    PackContentPresence::Present
                ),
                (
                    "datapacks/loose.zip".to_owned(),
                    AuxiliaryKind::DataPack,
                    Some(DataPackOwner::NoWorld),
                    PackContentPresence::Present
                ),
            ],
            "the server-only data pack is not the client's"
        );
        assert_eq!(
            snapshot.source.map(|source| source.name),
            Some("Reproduction Pack".to_owned())
        );
    }

    #[tokio::test]
    async fn a_pack_removed_from_the_game_directory_is_reported_absent_not_forgotten() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance) = imported_instance(base.path()).await;
        let root = app.client(instance).expect("record").root;
        std::fs::remove_file(
            ephervex_storage::metadata::client_game_dir(&root).join("resourcepacks/Faithful.zip"),
        )
        .expect("remove");

        let snapshot = app
            .host_pack_content(InstalledHost::Instance(instance))
            .expect("snapshot");
        let faithful = snapshot
            .entries
            .iter()
            .find(|entry| entry.name() == "Faithful.zip")
            .expect("still listed");
        assert_eq!(faithful.presence, PackContentPresence::Absent);
    }
}
