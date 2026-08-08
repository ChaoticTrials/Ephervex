//! The import pipeline: archive in, materialized side plus retained recipe out
//!
//! # Shape of the pipeline
//!
//! ```text
//! detect -> parse+validate -> analyze   (no writes; pack analysis)
//!                              |
//!                              v
//!        pack files -> overrides -> side-overrides -> retain source
//! ```
//!
//! [`analyze`] is the half that touches nothing: it opens the archive, reads
//! the manifest, and answers "what is this, and what would importing it do".
//! the import workflow shows that before the user commits, the CLI prints it for
//! `--inspect`, and — because it is also the half that refuses an unsupported
//! loader — nothing gets created for a pack that cannot be reproduced.
//!
//! [`materialize`] is the half that writes, and it writes in the required order. It
//! stops short of applying the global Minecraft preferences: those need the
//! configuration file, which lives above this crate, and they must come *after*
//! everything here.
//!
//! # Why detection and orchestration are provider-independent
//!
//! Only [`parse_with`] knows a provider exists, and it dispatches on a
//! [`PackFormat`]. Adding CurseForge is a second arm there plus a module beside
//! `modrinth`; nothing in the pipeline, the recipe, the installer, or the
//! retained-source layout has to learn a new vocabulary.

use std::path::{Path, PathBuf};

use ephervex_download::{CacheLayout, DownloadManager, OperationReporter};
use ephervex_model::{ImportSourceKind, LoaderSelection};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument};

use crate::archive::PackArchive;
use crate::detect::PackFormat;
use crate::error::{PackError, PackResult};
use crate::install::{self, AppliedLayer, MaterializedFiles};
use crate::modrinth::{self, ParsedPack};
use crate::policy::PackPolicy;
use crate::recipe::{
    OptionalComponent, OptionalSelection, OverrideScope, PackRecipe, PackSide, SideRequirement,
};
use crate::store::SourceStore;

/// What an archive turned out to be, before anything is written (pack analysis).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackAnalysis {
    pub format: PackFormat,
    pub recipe: PackRecipe,
    /// Download addresses the URL policy refused, for files that had another
    /// permitted address.
    pub rejected_urls: Vec<String>,
}

impl PackAnalysis {
    /// The pack's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.recipe.original_name
    }

    #[must_use]
    pub fn minecraft_version(&self) -> &str {
        &self.recipe.minecraft_version
    }

    #[must_use]
    pub fn loader(&self) -> &LoaderSelection {
        &self.recipe.loader
    }

    /// The optional files this side would ask about (optional-file selection).
    #[must_use]
    pub fn optional_components(&self, side: PackSide) -> Vec<OptionalComponent> {
        self.recipe.optional_components(side)
    }

    /// How many files this side installs without asking.
    #[must_use]
    pub fn required_file_count(&self, side: PackSide) -> usize {
        self.recipe
            .files
            .iter()
            .filter(|file| file.requirement(side) == SideRequirement::Required)
            .count()
    }

    /// Whether the pack carries anything for the side it is *not* being
    /// imported as.
    ///
    /// Worth surfacing because it is what makes "Create Server" meaningful
    /// later: a pack with server-only files preserved but not installed is the
    /// normal outcome of a client import.
    #[must_use]
    pub fn has_files_for(&self, side: PackSide) -> bool {
        self.recipe
            .files
            .iter()
            .any(|file| file.requirement(side).is_installable())
    }

    /// Which override layers the archive carries.
    pub fn layers(&self) -> impl Iterator<Item = OverrideScope> + '_ {
        self.recipe.override_layers.iter().map(|layer| layer.scope)
    }
}

/// Opens an archive and works out what it is, writing nothing.
///
/// The archive is returned alongside the analysis so a caller that goes on to
/// import does not re-open the file — which would let it be swapped between the
/// decision and the extraction.
#[instrument(skip(policy), fields(archive = %path.display()))]
pub fn analyze(path: &Path, policy: &PackPolicy) -> PackResult<(PackArchive, PackAnalysis)> {
    let mut archive = PackArchive::open(path, policy.limits)?;
    let Some(format) = archive.format() else {
        return Err(PackError::FormatUnrecognized {
            path: path.to_path_buf(),
        });
    };
    let analysis = parse_with(format, &mut archive, policy)?;
    info!(
        format = %analysis.format,
        pack = %analysis.recipe.original_name,
        minecraft = %analysis.recipe.minecraft_version,
        loader = %analysis.recipe.loader,
        files = analysis.recipe.files.len(),
        "analyzed a pack archive"
    );
    Ok((archive, analysis))
}

/// Parses an already-identified archive with the provider that owns its format.
///
/// The single dispatch point. A recognized but unsupported format is reported
/// as exactly that rather than as a parse failure.
pub fn parse_with(
    format: PackFormat,
    archive: &mut PackArchive,
    policy: &PackPolicy,
) -> PackResult<PackAnalysis> {
    match format {
        PackFormat::ModrinthPack => {
            let ParsedPack {
                recipe,
                manifest: _,
                rejected_urls,
            } = modrinth::read(archive, &policy.allowlist)?;
            Ok(PackAnalysis {
                format,
                recipe,
                rejected_urls,
            })
        }
        PackFormat::CurseForgePack | PackFormat::PrismExport => {
            Err(PackError::FormatUnsupported { format })
        }
    }
}

/// Everything an import wants to know once the files are on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedPack {
    pub side: PackSide,
    pub files: MaterializedFiles,
    pub layers: Vec<AppliedLayer>,
    /// The recipe as retained, with each layer's retention recorded and the
    /// user's optional-file decisions written into it.
    pub recipe: PackRecipe,
    /// Where the normalized recipe was written.
    pub recipe_path: PathBuf,
}

/// One request to materialize a side of an analyzed pack.
///
/// `game_dir` is the directory the game runs in; `source_dir` is the instance's
/// `.launcher/source/`. Both are supplied by the caller because this crate does
/// not know what an instance is.
#[derive(Debug)]
pub struct MaterializeRequest<'a> {
    pub archive: &'a mut PackArchive,
    pub analysis: &'a PackAnalysis,
    pub side: PackSide,
    pub selection: &'a OptionalSelection,
    pub game_dir: &'a Path,
    pub source_dir: &'a Path,
    pub manager: &'a DownloadManager,
    pub cache: &'a CacheLayout,
    pub reporter: &'a OperationReporter,
    pub cancel: &'a CancellationToken,
}

/// Materializes one side of an analyzed pack (the project's order).
#[instrument(skip_all, fields(side = request.side.as_str(), game_dir = %request.game_dir.display()))]
pub async fn materialize(request: MaterializeRequest<'_>) -> PackResult<MaterializedPack> {
    let MaterializeRequest {
        archive,
        analysis,
        side,
        selection,
        game_dir,
        source_dir,
        manager,
        cache,
        reporter,
        cancel,
    } = request;

    if cancel.is_cancelled() {
        return Err(PackError::Canceled);
    }

    let mut recipe = analysis.recipe.clone();
    // Recorded before anything is installed, so the recipe describes the
    // decision the import was made under even if a later step fails.
    recipe.record_selection(side, selection);

    // 1. Pack files.
    let files = install::install_files(install::FileInstall {
        recipe: &recipe,
        side,
        selection,
        game_dir,
        manager,
        cache,
        reporter,
        cancel,
    })
    .await?;

    // 2. Overrides, then side-overrides. Later layer wins.
    let layers = install::apply_overrides(archive, &recipe, side, game_dir)?;
    for applied in &layers {
        if let Some(layer) = recipe
            .override_layers
            .iter_mut()
            .find(|layer| layer.scope == applied.layer.scope)
        {
            layer.files = applied.files;
        }
    }

    // 3. Retain the source recipe, which is mandatory even for a client-only
    //    import.
    let store = SourceStore::new(source_dir);
    retain_source(archive, &mut recipe, side, &store)?;
    let recipe_path = store.write_recipe(&recipe)?;

    info!(
        installed = files.installed.len(),
        preserved = files.skipped.len(),
        layers = layers.len(),
        "materialized a pack"
    );
    Ok(MaterializedPack {
        side,
        files,
        layers,
        recipe,
        recipe_path,
    })
}

/// Keeps everything a server created later will need.
///
/// The provider's manifest is kept byte for byte, and every override layer the
/// *other* side needs is copied out of the archive before it goes away. The
/// layer this side just extracted into the game directory is not duplicated:
/// A modpack's overrides are often the bulk of its bytes, so the layer already
/// installed for this side is not duplicated.
fn retain_source(
    archive: &mut PackArchive,
    recipe: &mut PackRecipe,
    side: PackSide,
    store: &SourceStore,
) -> PackResult<()> {
    let kind = recipe.source_kind;
    let manifest_entry = manifest_entry_for(kind);
    if let Some(entry) = manifest_entry {
        let bytes = archive.read_entry(entry)?;
        let relative = store.retain_manifest(kind, entry, &bytes)?;
        recipe.provider_metadata.manifest_file = Some(relative);
    }

    let already_applied = side.override_scope();
    for layer in &mut recipe.override_layers {
        if layer.scope == already_applied {
            // Already in the game directory, so it does not need to be
            // duplicated. The recipe records that it was not retained so a
            // later plan can say so rather than assume.
            layer.retained_at = None;
            continue;
        }
        let (absolute, relative) = store.layer_dir(kind, &layer.archive_prefix);
        let report = archive.extract_prefix(&layer.archive_prefix, &absolute)?;
        layer.files = report.files;
        layer.retained_at = Some(relative);
    }
    Ok(())
}

/// The manifest file name one provider's retained state keeps.
const fn manifest_entry_for(kind: ImportSourceKind) -> Option<&'static str> {
    match kind {
        ImportSourceKind::ModrinthPack => Some(crate::detect::MODRINTH_INDEX),
        ImportSourceKind::CurseForgePack => Some(crate::detect::CURSEFORGE_MANIFEST),
        ImportSourceKind::PrismExport => Some(crate::detect::PRISM_PACK),
        ImportSourceKind::Manual => None,
    }
}

/// Reads back the recipe an instance retained.
///
/// This is what "Create Server" is built on: it never touches the original
/// archive, which by then is usually gone, and never infers anything from the
/// client's current `mods/` folder, which by then has been edited.
pub fn retained_recipe(source_dir: &Path) -> PackResult<PackRecipe> {
    SourceStore::new(source_dir).read_recipe()
}

/// Whether an instance retained a pack recipe at all.
#[must_use]
pub fn has_retained_recipe(source_dir: &Path) -> bool {
    SourceStore::new(source_dir).has_recipe()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::io::Write as _;

    use super::*;

    fn zip_file(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            writer.start_file(*name, options).expect("start entry");
            writer.write_all(body).expect("write entry");
        }
        writer.finish().expect("finish");
    }

    const INDEX: &[u8] = br#"{
        "formatVersion": 1,
        "game": "minecraft",
        "versionId": "1.2.3",
        "name": "Analysis Pack",
        "files": [],
        "dependencies": {"minecraft": "1.20.1", "neoforge": "47.1.106"}
    }"#;

    #[test]
    fn analysis_reads_the_pack_without_writing_anything() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        zip_file(
            &archive,
            &[
                ("modrinth.index.json", INDEX),
                ("overrides/config/a.toml", b"a"),
                ("server-overrides/config/b.toml", b"b"),
            ],
        );

        let (_, analysis) = analyze(&archive, &PackPolicy::modrinth()).expect("analyze");
        assert_eq!(analysis.format, PackFormat::ModrinthPack);
        assert_eq!(analysis.name(), "Analysis Pack");
        assert_eq!(analysis.minecraft_version(), "1.20.1");
        assert_eq!(analysis.loader().to_string(), "NeoForge 47.1.106");

        let layers: Vec<OverrideScope> = analysis.layers().collect();
        assert_eq!(layers, vec![OverrideScope::Common, OverrideScope::Server]);

        // Nothing beside the archive was created.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn a_recognized_but_unsupported_format_is_not_reported_as_unrecognized() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.zip");
        zip_file(
            &archive,
            &[
                ("manifest.json", br#"{"minecraft":{"version":"1.20.1"}}"#),
                ("overrides/mods/a.jar", b"jar"),
            ],
        );

        let error = analyze(&archive, &PackPolicy::default()).expect_err("unsupported format");
        assert!(matches!(
            error,
            PackError::FormatUnsupported {
                format: PackFormat::CurseForgePack
            }
        ));
        assert_eq!(error.message_id(), "error-pack-format-unsupported");
    }

    #[test]
    fn an_archive_that_is_no_pack_at_all_names_the_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("photos.zip");
        zip_file(&archive, &[("holiday.jpg", b"not a pack")]);

        let error = analyze(&archive, &PackPolicy::default()).expect_err("unrecognized");
        assert!(matches!(error, PackError::FormatUnrecognized { .. }));
        assert_eq!(error.failing_path(), Some(archive));
    }

    #[tokio::test]
    async fn a_client_import_retains_the_layers_a_server_will_need_and_not_the_one_it_has() {
        // Common and server layers survive after the source archive is removed,
        // the client layer is not duplicated, and the manifest is kept verbatim.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive_path = dir.path().join("pack.mrpack");
        zip_file(
            &archive_path,
            &[
                ("modrinth.index.json", INDEX),
                ("overrides/config/common.toml", b"common"),
                ("client-overrides/options.txt", b"guiScale:1\n"),
                ("server-overrides/server.properties", b"motd=test\n"),
            ],
        );

        let (mut archive, analysis) =
            analyze(&archive_path, &PackPolicy::modrinth()).expect("analyze");
        let game = dir.path().join("instance/minecraft");
        let source = dir.path().join("instance/.launcher/source");

        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let materialized = materialize(MaterializeRequest {
            archive: &mut archive,
            analysis: &analysis,
            side: PackSide::Client,
            selection: &OptionalSelection::none(),
            game_dir: &game,
            source_dir: &source,
            manager: &manager,
            cache: &CacheLayout::new(dir.path().join("cache")),
            reporter: &OperationReporter::silent("test-import"),
            cancel: &CancellationToken::new(),
        })
        .await
        .expect("materialize");

        // The client got both of its layers, in order.
        assert_eq!(
            std::fs::read_to_string(game.join("config/common.toml")).expect("read"),
            "common"
        );
        assert!(game.join("options.txt").is_file());
        assert!(!game.join("server.properties").exists());

        // The server's layers survive under `.launcher/source/`.
        assert!(
            source
                .join("modrinth/server-overrides/server.properties")
                .is_file()
        );
        assert!(
            source
                .join("modrinth/overrides/config/common.toml")
                .is_file()
        );
        assert!(
            !source.join("modrinth/client-overrides").exists(),
            "the client layer is installed directly and is not retained twice"
        );
        assert_eq!(
            std::fs::read(source.join("modrinth/modrinth.index.json")).expect("read"),
            INDEX,
            "the original manifest is retained byte for byte"
        );

        //...and the recipe records which layers can still be applied.
        let recipe = retained_recipe(&source).expect("read back");
        assert_eq!(recipe, materialized.recipe);
        let server_plan = recipe.plan(PackSide::Server, &OptionalSelection::none());
        assert!(server_plan.layers_are_retained());
        assert!(
            !recipe
                .plan(PackSide::Client, &OptionalSelection::none())
                .layers_are_retained()
        );
    }
}
