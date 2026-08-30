//! Importing a pack into an instance.
//!
//! # What this layer adds over `ephervex-pack`
//!
//! `ephervex-pack` knows how to read an archive, validate it, and lay its files
//! down in a directory. It deliberately does not know what an instance is, where
//! instances live, or that global Minecraft preferences exist. Sequencing those
//! is this module's whole job. The sequence is:
//!
//! ```text
//! pack files -> overrides -> client-overrides -> global managed preferences
//! ```
//!
//! The last step is why [`crate::App::create_client`] is *not* used to create an
//! imported instance: it applies the preferences at creation, which would put
//! them before the pack's own `options.txt` instead of after it. The registry is
//! called directly and [`crate::App::apply_global_preferences`] is called at the
//! end, preserving the same one-time application after pack options are in place.
//!
//! # An import either produces an instance or leaves nothing behind
//!
//! Validation happens before anything is created, so a pack requiring Fabric
//! never reaches the point of having a directory. If materialization
//! fails afterwards — a download that will not verify, a full disk — the
//! half-imported instance is removed rather than left looking importable. A
//! directory containing three of a pack's forty mods is not a reproduction of
//! anything, and the launcher should not offer to launch one.

use std::path::{Path, PathBuf};

use ephervex_download::{CacheLayout, OperationReporter};
use ephervex_minecraft::{MinecraftError, MinecraftVersionId};
use ephervex_model::{InstanceId, StorageClass};
use ephervex_options::PreferenceApplication;
use ephervex_pack::{
    MaterializeRequest, MaterializedPack, OptionalSelection, PackAnalysis, PackArchive, PackPolicy,
    PackRecipe, PackSide, ProviderFileIdentity, SourceStore,
};
use ephervex_storage::{ClientRecord, ImportSource, NewClient};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::app::App;
use crate::curseforge_pack::ProviderCredentials;
use crate::error::CoreResult;

/// What a caller wants imported.
#[derive(Debug, Clone)]
pub struct ImportRequest {
    /// The archive to import. Detected by content, not by extension.
    pub archive: PathBuf,
    /// The instance's name, or `None` for the pack's own name.
    pub name: Option<String>,
    /// Where the instance lives. Temporary by default, like everything else
    /// this launcher creates.
    pub storage: StorageClass,
    /// Which optional files to take. Empty means none, which is the product
    /// default.
    pub selection: OptionalSelection,
    /// Provider-refused files the caller has explicitly decided to do without.
    ///
    /// Empty means none, which is the product default and the only value the
    /// CLI ever supplies: import still refuses atomically when a pack has any
    /// file a provider refused and nothing said what to do about it.
    pub skip_blocked: Vec<ProviderFileIdentity>,
}

impl ImportRequest {
    /// An import of one archive with every default in place.
    #[must_use]
    pub fn new(archive: impl Into<PathBuf>) -> Self {
        Self {
            archive: archive.into(),
            name: None,
            storage: StorageClass::default_for_new_instance(),
            selection: OptionalSelection::none(),
            skip_blocked: Vec::new(),
        }
    }

    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    #[must_use]
    pub const fn with_storage(mut self, storage: StorageClass) -> Self {
        self.storage = storage;
        self
    }

    #[must_use]
    pub fn selecting(mut self, selection: OptionalSelection) -> Self {
        self.selection = selection;
        self
    }

    #[must_use]
    pub fn skipping_blocked(mut self, skip_blocked: Vec<ProviderFileIdentity>) -> Self {
        self.skip_blocked = skip_blocked;
        self
    }
}

/// What an import produced.
#[derive(Debug)]
pub struct ImportedPack {
    pub record: ClientRecord,
    /// What was installed, preserved, and layered.
    pub materialized: MaterializedPack,
    /// The analysis the import ran on, including any refused download address.
    pub analysis: PackAnalysis,
    /// The one-time global preference application, or `None` if
    /// it failed — reported, never fatal: an instance that exists with a pack's
    /// own settings is a far better outcome than an import undone over a GUI
    /// scale.
    pub preferences: Option<PreferenceApplication>,
}

impl ImportedPack {
    /// The retained recipe a server can later be built from.
    #[must_use]
    pub const fn recipe(&self) -> &PackRecipe {
        &self.materialized.recipe
    }
}

/// One request to lay an analyzed pack down into an instance.
///
/// A struct rather than seven positional arguments, and the field that made it
/// worth having is `credentials`: a caller assembling it has to say out loud
/// where a credential comes from if one of this pack's files turns out to need
/// one.
#[derive(Debug)]
struct MaterializePack<'a> {
    archive: &'a mut PackArchive,
    analysis: &'a PackAnalysis,
    record: &'a ClientRecord,
    selection: &'a OptionalSelection,
    credentials: &'a ProviderCredentials<'a>,
    reporter: &'a OperationReporter,
    cancel: &'a CancellationToken,
}

impl App {
    /// Reads a pack archive and reports what importing it would do (pack analysis).
    ///
    /// Writes nothing, contacts nobody, and refuses a pack this launcher cannot
    /// reproduce — an unsupported loader, an ambiguous one, a Minecraft release
    /// below the supported floor — before the user commits to anything.
    #[instrument(skip(self), fields(archive = %archive.display()))]
    pub fn analyze_pack(&self, archive: &Path) -> CoreResult<PackAnalysis> {
        let (_, analysis) = Self::open_pack(archive)?;
        Ok(analysis)
    }

    /// Imports a pack as a client instance.
    ///
    /// Server materialization belongs to `ephervex-server`, which owns
    /// dedicated-server installation and EULA handling. Import retains the
    /// recipe that operation needs, and [`Self::pack_recipe`] exposes it
    /// without requiring the original archive.
    #[instrument(
        skip(self, request, reporter, cancel),
        fields(archive = %request.archive.display(), operation = %reporter.id())
    )]
    pub async fn import_pack(
        &self,
        request: &ImportRequest,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ImportedPack> {
        // An import fetches pack files into the shared cache and then copies
        // each one into the instance, so it must hold the artifact-cache lease
        // across both halves.
        self.with_artifact_cache(Box::pin(
            self.import_pack_holding_the_cache(request, reporter, cancel),
        ))
        .await
    }

    async fn import_pack_holding_the_cache(
        &self,
        request: &ImportRequest,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ImportedPack> {
        // Everything that can refuse the pack runs before the instance exists.
        reporter.set_stage(Some("operation-stage-reading-archive".to_owned()));
        let (mut archive, mut analysis) = Self::open_pack(&request.archive)?;
        // Including the provider lookups: a missing API key, a file the pack
        // declares that no longer exists, and a file whose author does not
        // permit third-party downloads all fail here, with nothing created.
        reporter.set_stage(Some("operation-stage-resolving-files".to_owned()));
        let authenticated = self
            .prepare_provider_files(&mut analysis, &request.skip_blocked, cancel)
            .await?;
        // The resolution above already read whatever secret store this pack's
        // provider needs, so the download half reuses that value rather than
        // reading it again.
        let credentials = match authenticated {
            Some(credential) => self.provider_credentials().holding_curseforge(credential),
            None => self.provider_credentials(),
        };

        let name = request
            .name
            .clone()
            .unwrap_or_else(|| analysis.recipe.original_name.clone());
        let record = self.registry_for_services().create_client(
            NewClient::new(
                name,
                analysis.recipe.minecraft_version.clone(),
                analysis.recipe.loader.clone(),
            )
            .with_storage(request.storage)
            .with_source(import_source(&analysis))
            .with_attention(analysis.attention.clone()),
        )?;

        let materialized = match self
            .materialize(MaterializePack {
                archive: &mut archive,
                analysis: &analysis,
                record: &record,
                selection: &request.selection,
                credentials: &credentials,
                reporter,
                cancel,
            })
            .await
        {
            Ok(materialized) => materialized,
            Err(error) => {
                // A partial import is not a reproduction of anything. The
                // instance was never published, so there is no row to delete
                // and no other operation could have observed it.
                crate::app::discard_unpublished(self.registry_for_services(), &record);
                return Err(error);
            }
        };

        // the managed global keys are applied *after* the pack's own
        // overrides, so a pack's `options.txt` survives except where the user
        // has explicitly asked the launcher to manage a key.
        reporter.set_stage(Some("operation-stage-applying-settings".to_owned()));
        let context = self.preference_context(&record.minecraft_version).await;
        let preferences = match self.apply_global_preferences(&record, &context) {
            Ok(applied) => Some(applied),
            Err(error) => {
                warn!(
                    instance_id = %record.id,
                    %error,
                    "imported the pack without applying the global Minecraft preferences"
                );
                None
            }
        };

        reporter.set_stage(Some("operation-stage-publishing".to_owned()));
        if let Err(error) = self.registry_for_services().publish_client(&record) {
            crate::app::discard_unpublished(self.registry_for_services(), &record);
            return Err(error.into());
        }

        info!(
            instance_id = %record.id,
            pack = %analysis.recipe.original_name,
            installed = materialized.files.installed.len(),
            preserved = materialized.files.skipped.len(),
            "imported a pack"
        );
        Ok(ImportedPack {
            record,
            materialized,
            analysis,
            preferences,
        })
    }

    /// The pack recipe an instance retained at import time.
    ///
    /// Read from the instance's own `.launcher/source/`, never from the
    /// original archive and never inferred from the current `mods/` folder —
    /// which is the whole point of retaining it.
    pub fn pack_recipe(&self, instance: InstanceId) -> CoreResult<Option<PackRecipe>> {
        let record = self.client(instance)?;
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&record.root));
        if !store.has_recipe() {
            // Not an error: an instance created by hand has no pack to record.
            return Ok(None);
        }
        Ok(Some(store.read_recipe()?))
    }

    /// Where an instance's retained source lives, for a frontend that wants to
    /// show or open it.
    pub fn pack_source_dir(&self, instance: InstanceId) -> CoreResult<PathBuf> {
        Ok(ephervex_storage::metadata::source_dir(
            &self.client(instance)?.root,
        ))
    }

    /// Lays the pack's files down and retains its source recipe.
    async fn materialize(&self, request: MaterializePack<'_>) -> CoreResult<MaterializedPack> {
        let MaterializePack {
            archive,
            analysis,
            record,
            selection,
            credentials,
            reporter,
            cancel,
        } = request;
        let manager = self.artifact_manager()?;

        Ok(ephervex_pack::materialize(MaterializeRequest {
            archive,
            analysis,
            side: PackSide::Client,
            selection,
            game_dir: &ephervex_storage::metadata::client_game_dir(&record.root),
            source_dir: &ephervex_storage::metadata::source_dir(&record.root),
            manager: &manager,
            cache: &CacheLayout::new(self.paths().cache_dir()),
            credentials,
            reporter,
            cancel,
        })
        .await?)
    }

    /// Opens and validates an archive under this installation's import policy.
    fn open_pack(archive: &Path) -> CoreResult<(PackArchive, PackAnalysis)> {
        let (opened, analysis) = ephervex_pack::analyze(archive, &Self::pack_policy())?;
        check_supported_release(&analysis.recipe.minecraft_version)?;
        Ok((opened, analysis))
    }

    /// The rules imports run under.
    ///
    /// Derived from the format so each provider gets its own download
    /// allowlist, and a hook for a future setting that widens the extraction
    /// limits for an unusually large pack.
    fn pack_policy() -> PackPolicy {
        PackPolicy::default()
    }
}

/// Refuses a Minecraft release below the supported floor, offline.
///
/// Deliberately decided from the version id alone rather than by asking Mojang:
/// an import should fail on a 1.7.10 pack in a second and without a network,
/// and a version id in a numbering scheme this build does not recognize is left
/// to the launch path — which resolves it against the manifest and refuses it
/// there if it is not a real release.
fn check_supported_release(version: &str) -> CoreResult<()> {
    let id = MinecraftVersionId::parse(version);
    if id.is_supported() == Some(false) {
        return Err(MinecraftError::unsupported(version).into());
    }
    Ok(())
}

/// How the instance records where it came from.
///
/// The reference is untranslated technical text. The archive's file name is
/// preferred because it is what the user chose; the pack's own name is the
/// fallback for an archive read from a path with no usable file name.
fn import_source(analysis: &PackAnalysis) -> ImportSource {
    let reference = analysis
        .recipe
        .archive_name
        .clone()
        .unwrap_or_else(|| analysis.recipe.original_name.clone());
    ImportSource::imported(analysis.format.source_kind(), reference)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::io::Write as _;

    use ephervex_model::{ImportSourceKind, LoaderKind};
    use ephervex_options::{OPTIONS_FILE_NAME, PreferenceKey};
    use ephervex_storage::Paths;

    use super::*;

    fn app_rooted_at(base: &Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app opens")
    }

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

    /// An index with no `files` at all, so the whole import is offline.
    fn index(dependencies: &str) -> Vec<u8> {
        format!(
            r#"{{
                "formatVersion": 1,
                "game": "minecraft",
                "versionId": "2.1.0",
                "name": "Overrides Only",
                "files": [],
                "dependencies": {dependencies}
            }}"#
        )
        .into_bytes()
    }

    async fn import(app: &App, archive: &Path) -> CoreResult<ImportedPack> {
        app.import_pack(
            &ImportRequest::new(archive),
            &OperationReporter::silent("test-import"),
            &CancellationToken::new(),
        )
        .await
    }

    #[tokio::test]
    async fn a_pack_import_applies_managed_preferences_after_the_packs_own_options() {
        // Apply preferences after pack overrides so managed preferences win conflicts.
        // preference": the managed key wins once, and every other line the pack
        // shipped survives.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        zip_file(
            &archive,
            &[
                (
                    "modrinth.index.json",
                    &index(r#"{"minecraft": "1.20.1", "neoforge": "47.1.106"}"#),
                ),
                (
                    "overrides/options.txt",
                    b"guiScale:1\nrenderDistance:12\n" as &[u8],
                ),
                (
                    "client-overrides/options.txt",
                    b"guiScale:4\nfov:0.5\njei:configVersion:2\n",
                ),
                ("server-overrides/server.properties", b"motd=repro\n"),
            ],
        );

        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.set_minecraft_preference(PreferenceKey::GuiScale, "3")
            .expect("set");

        let imported = import(&app, &archive).await.expect("import");
        let options = std::fs::read_to_string(
            ephervex_storage::metadata::client_game_dir(&imported.record.root)
                .join(OPTIONS_FILE_NAME),
        )
        .expect("options");

        assert!(
            options.contains("guiScale:3"),
            "the managed global key wins over both override layers: {options}"
        );
        assert!(
            options.contains("fov:0.5") && options.contains("jei:configVersion:2"),
            "every unmanaged key the last layer shipped survived, mod keys included: {options}"
        );
        assert!(
            !options.contains("renderDistance:12"),
            "a later override layer replaces the earlier file wholesale: {options}"
        );
    }

    #[tokio::test]
    async fn an_imported_instance_records_its_source_and_the_packs_exact_versions() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("report.mrpack");
        zip_file(
            &archive,
            &[(
                "modrinth.index.json",
                &index(r#"{"minecraft": "1.20.1", "forge": "47.4.0"}"#),
            )],
        );

        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let imported = import(&app, &archive).await.expect("import");

        assert_eq!(imported.record.name, "Overrides Only");
        assert_eq!(imported.record.minecraft_version, "1.20.1");
        assert_eq!(imported.record.loader.kind, LoaderKind::Forge);
        assert_eq!(
            imported.record.loader.version.as_deref(),
            Some("47.4.0"),
            "the exact declared build, never a floating latest"
        );
        assert_eq!(imported.record.source.kind, ImportSourceKind::ModrinthPack);
        assert_eq!(
            imported.record.source.reference.as_deref(),
            Some("report.mrpack")
        );
        assert_eq!(imported.record.storage, StorageClass::Temporary);

        // The record survives a re-read: this is what the index and the
        // metadata file agreeing looks like.
        let stored = app.client(imported.record.id).expect("read back");
        assert_eq!(stored.source, imported.record.source);
    }

    #[tokio::test]
    async fn a_pack_requiring_fabric_is_refused_and_leaves_no_instance_behind() {
        // "Pack requires Fabric — reject as unsupported loader."
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("fabric.mrpack");
        zip_file(
            &archive,
            &[(
                "modrinth.index.json",
                &index(r#"{"minecraft": "1.20.1", "fabric-loader": "0.15.7"}"#),
            )],
        );

        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let error = import(&app, &archive).await.expect_err("must refuse");

        assert_eq!(error.message_id(), "error-pack-loader-unsupported");
        assert_eq!(error.unsupported_loader(), Some("fabric-loader"));
        assert!(
            app.list_clients().expect("list").is_empty(),
            "nothing is created for a pack that cannot be reproduced"
        );
    }

    #[tokio::test]
    async fn a_pack_for_a_release_older_than_ephervex_supports_is_refused_offline() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("old.mrpack");
        zip_file(
            &archive,
            &[(
                "modrinth.index.json",
                &index(r#"{"minecraft": "1.7.10", "forge": "10.13.4.1614"}"#),
            )],
        );

        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-minecraft-version-unsupported");
        assert!(app.list_clients().expect("list").is_empty());
    }

    #[tokio::test]
    async fn an_instance_created_by_hand_has_no_recipe_which_is_not_a_failure() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = app
            .create_client_for_test(NewClient::new(
                "By hand",
                "1.20.1",
                ephervex_model::LoaderSelection::vanilla(),
            ))
            .expect("create");
        assert_eq!(app.pack_recipe(record.id).expect("query"), None);
    }

    #[tokio::test]
    async fn the_server_recipe_is_readable_from_the_instance_without_the_archive() {
        // The Modrinth pack import required behavior's second half, at the services
        // layer: the archive is deleted, and the server recipe is still there.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        zip_file(
            &archive,
            &[
                (
                    "modrinth.index.json",
                    &index(r#"{"minecraft": "1.20.1", "neoforge": "47.1.106"}"#),
                ),
                ("overrides/config/common.toml", b"common"),
                ("server-overrides/server.properties", b"motd=repro\n"),
            ],
        );

        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let imported = import(&app, &archive).await.expect("import");
        std::fs::remove_file(&archive).expect("the archive goes away");

        let recipe = app
            .pack_recipe(imported.record.id)
            .expect("query")
            .expect("an imported instance has one");
        let plan = recipe.plan(PackSide::Server, &OptionalSelection::none());
        assert!(plan.layers_are_retained());
        assert_eq!(plan.minecraft_version, "1.20.1");

        let source = app.pack_source_dir(imported.record.id).expect("source dir");
        assert!(
            source
                .join("modrinth/server-overrides/server.properties")
                .is_file()
        );
    }

    #[test]
    fn the_extractor_and_the_storage_layout_agree_on_which_directory_is_reserved() {
        // `ephervex-pack` refuses to write into `.launcher` without depending
        // on `ephervex-storage` to learn the name. This is the assertion that
        // keeps the two definitions from drifting apart.
        assert_eq!(
            ephervex_pack::RESERVED_METADATA_DIR,
            ephervex_storage::paths::LAUNCHER_METADATA_DIR
        );
    }
}
