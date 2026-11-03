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
//! fails afterwards - a download that will not verify, a full disk - the
//! half-imported instance is removed rather than left looking importable. A
//! directory containing three of a pack's forty mods is not a reproduction of
//! anything, and the launcher should not offer to launch one.

use std::path::{Path, PathBuf};

use ephervex_download::{CacheLayout, OperationReporter};
use ephervex_minecraft::{MinecraftError, MinecraftVersionId};
use ephervex_model::{InstanceId, StorageClass};
use ephervex_options::PreferenceApplication;
use ephervex_pack::{
    MaterializeRequest, MaterializedPack, OptionalSelection, PackAnalysis, PackArchive, PackError,
    PackPolicy, PackRecipe, PackSide, ProviderFileIdentity, RecipeMaterializeRequest, SourceStore,
};
use ephervex_storage::{ClientRecord, ImportSource, NewClient};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::app::App;
use crate::curseforge_pack::ProviderCredentials;
use crate::error::CoreResult;
use crate::instance::{sanitized_entity_name, validated_entity_name};
use crate::issue_link::validated_issue_link;

/// What a caller wants imported.
#[derive(Debug, Clone)]
pub struct ImportRequest {
    /// The archive to import. Detected by content, not by extension.
    pub archive: PathBuf,
    /// The instance's name, or `None` for the pack's own name.
    ///
    /// A name given here is refused when a rename would refuse it. The pack's
    /// own name is cleaned up instead, because the user did not write it.
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
    /// The issue or bug report this pack reproduces, stored on the instance.
    ///
    /// Checked the way a link typed into the instance is, and before any
    /// archive or network work: a refused one creates nothing.
    pub issue_link: Option<String>,
    /// The id the instance takes, or `None` for a fresh one. A frontend names
    /// it to address the instance while the import is still running; one
    /// that already names an instance is refused.
    pub instance_id: Option<InstanceId>,
    /// Whether to make the instance match a hashed mod list the pack carries
    /// in its overrides. Off by default, because it changes what the pack
    /// installs. A crash report or log imported on its own is always used.
    pub logged_mods: bool,
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
            issue_link: None,
            instance_id: None,
            logged_mods: false,
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

    #[must_use]
    pub fn with_issue_link(mut self, issue_link: impl Into<String>) -> Self {
        self.issue_link = Some(issue_link.into());
        self
    }

    /// Makes the instance match the hashed mod list the pack carries, when it
    /// carries one.
    #[must_use]
    pub const fn applying_logged_mods(mut self, apply: bool) -> Self {
        self.logged_mods = apply;
        self
    }

    #[must_use]
    pub const fn with_instance_id(mut self, id: InstanceId) -> Self {
        self.instance_id = Some(id);
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
    /// it failed - reported, never fatal: an instance that exists with a pack's
    /// own settings is a far better outcome than an import undone over a GUI
    /// scale.
    pub preferences: Option<PreferenceApplication>,
    /// What making the instance match a mod list the pack carries did to the
    /// pack's mods the list does not name.
    pub unlisted: crate::logged_mods::UnlistedMods,
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
    source: OpenedSource,
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
    /// reproduce - an unsupported loader, an ambiguous one, a Minecraft release
    /// below the supported floor - before the user commits to anything.
    #[instrument(skip(self), fields(archive = %archive.display()))]
    pub fn analyze_pack(&self, archive: &Path) -> CoreResult<PackAnalysis> {
        let (_, analysis) = Self::open_pack(archive)?;
        Ok(analysis)
    }

    /// What importing `request` would do, as [`Self::analyze_pack`] reports
    /// it, with a mod list the pack carries applied only when the request
    /// asks for it.
    pub fn analyze_import(&self, request: &ImportRequest) -> CoreResult<PackAnalysis> {
        let (_, analysis) = Self::open_for_import(request)?;
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
        // A name the caller typed is checked before any archive or network work.
        let chosen_name = request
            .name
            .as_deref()
            .map(validated_entity_name)
            .transpose()?;
        let issue_link = request
            .issue_link
            .as_deref()
            .map(validated_issue_link)
            .transpose()?
            .flatten();
        // Everything that can refuse the pack runs before the instance exists.
        reporter.set_stage(Some("operation-stage-reading-archive".to_owned()));
        let (source, mut analysis) = Self::open_for_import(request)?;
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

        let name = match chosen_name {
            Some(chosen) => chosen,
            None => suggested_instance_name(&analysis),
        };
        let mut new_client = NewClient::new(
            name,
            analysis.recipe.minecraft_version.clone(),
            analysis.recipe.loader.clone(),
        )
        .with_storage(request.storage)
        .with_source(import_source(&analysis))
        .with_attention(analysis.attention.clone());
        if let Some(issue_link) = issue_link {
            new_client = new_client.with_issue_link(issue_link);
        }
        new_client.id = request.instance_id;
        let record = self.registry_for_services().create_client(new_client)?;

        let (materialized, unlisted) = match self
            .materialize(MaterializePack {
                source,
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

        if cancel.is_cancelled() {
            crate::app::discard_unpublished(self.registry_for_services(), &record);
            return Err(PackError::Canceled.into());
        }
        reporter.set_stage(Some("operation-stage-publishing".to_owned()));
        if let Err(error) = self.registry_for_services().publish_client(&record) {
            crate::app::discard_unpublished(self.registry_for_services(), &record);
            return Err(error.into());
        }

        self.record_host_event(
            crate::mods::InstalledHost::Instance(record.id),
            ephervex_storage::HostEventKind::Created,
            crate::host_events::storage_event_subject(record.storage),
            "",
        );
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
            unlisted,
        })
    }

    /// The pack recipe an instance retained at import time.
    ///
    /// Read from the instance's own `.launcher/source/`, never from the
    /// original archive and never inferred from the current `mods/` folder -
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
    ///
    /// Returns the pack's mods it disabled because a mod list the instance
    /// was made to match does not name them.
    async fn materialize(
        &self,
        request: MaterializePack<'_>,
    ) -> CoreResult<(MaterializedPack, crate::logged_mods::UnlistedMods)> {
        let MaterializePack {
            source,
            analysis,
            record,
            selection,
            credentials,
            reporter,
            cancel,
        } = request;
        let manager = self.artifact_manager()?;
        let game_dir = ephervex_storage::metadata::client_game_dir(&record.root);
        let source_dir = ephervex_storage::metadata::source_dir(&record.root);
        let cache = CacheLayout::new(self.paths().cache_dir());

        match source {
            OpenedSource::Archive(archive) => {
                let materialized = ephervex_pack::materialize(MaterializeRequest {
                    archive,
                    analysis,
                    side: PackSide::Client,
                    selection,
                    game_dir: &game_dir,
                    source_dir: &source_dir,
                    manager: &manager,
                    cache: &cache,
                    credentials,
                    reporter,
                    cancel,
                })
                .await?;
                // A pack made to match a mod list keeps only the mods the
                // list names. The others are disabled rather than deleted, so
                // they can be turned back on.
                let disabled = match &analysis.logged_mods {
                    Some(logged) => {
                        crate::logged_mods::disable_unlisted_mods(
                            &game_dir,
                            logged,
                            &materialized.recipe,
                            cancel,
                        )
                        .await?
                    }
                    None => crate::logged_mods::UnlistedMods::default(),
                };
                Ok((materialized, disabled))
            }
            OpenedSource::ModList(report) => {
                let materialized = ephervex_pack::materialize_recipe(RecipeMaterializeRequest {
                    analysis,
                    side: PackSide::Client,
                    selection,
                    game_dir: &game_dir,
                    source_dir: &source_dir,
                    manager: &manager,
                    cache: &cache,
                    credentials,
                    reporter,
                    cancel,
                })
                .await?;
                if cancel.is_cancelled() {
                    return Err(PackError::Canceled.into());
                }
                keep_mod_list_source(&game_dir, analysis, report).await;
                Ok((materialized, crate::logged_mods::UnlistedMods::default()))
            }
        }
    }

    /// Opens what `request` imports, keeping a mod list the pack carries only
    /// when the request asks for it.
    fn open_for_import(request: &ImportRequest) -> CoreResult<(OpenedSource, PackAnalysis)> {
        let (mut source, mut analysis) = Self::open_pack(&request.archive)?;
        if let OpenedSource::Archive(archive) = &mut source {
            if request.logged_mods {
                if let Some(logged) = analysis.logged_mods.as_mut() {
                    logged.requested = true;
                    logged.shipped_jars = ephervex_pack::shipped_jars(archive, &analysis.recipe);
                }
            } else {
                analysis.logged_mods = None;
            }
        }
        Ok((source, analysis))
    }

    /// Opens and validates a pack archive, or a crash report or log, under
    /// this installation's import policy.
    fn open_pack(path: &Path) -> CoreResult<(OpenedSource, PackAnalysis)> {
        let as_mod_list = if ephervex_pack::reads_as_text(path)? {
            match ephervex_pack::analyze_logged_mod_list(path) {
                // Text that is no crash report either, such as an error page
                // saved in place of a pack, is diagnosed as the archive it was
                // meant to be.
                Err(PackError::FormatUnrecognized { .. }) => None,
                other => Some(other?),
            }
        } else {
            None
        };
        let (opened, analysis) = if let Some(analysis) = as_mod_list {
            (OpenedSource::ModList(path.to_path_buf()), analysis)
        } else {
            let (archive, analysis) = ephervex_pack::analyze(path, &Self::pack_policy())?;
            (OpenedSource::Archive(archive), analysis)
        };
        check_supported_release(&analysis.recipe.minecraft_version)?;
        // Decided from the pinned version alone, so a pack whose loader build
        // cannot be installed is refused before any file is downloaded.
        if let Some(exact) = analysis.recipe.loader.version.as_deref() {
            ephervex_loaders::require_supported_installer_format(
                analysis.recipe.loader.kind,
                &analysis.recipe.minecraft_version,
                exact,
            )?;
        }
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

/// What an import was opened from. An archive stays open, so the import uses
/// the content that was analyzed. A crash report or log is analyzed whole
/// before anything is created, and is only reopened for the copy the instance
/// keeps.
#[derive(Debug)]
enum OpenedSource {
    Archive(PackArchive),
    /// A crash report or log, kept by path and copied into the instance once
    /// it exists.
    ModList(PathBuf),
}

/// Copies the crash report or log an instance was rebuilt from into it, so
/// the logs view and crash review show it before the first launch.
///
/// A report keeps its own name when that is a crash report's name, and a log
/// becomes `logs/latest.log`. As much of the file is copied as was analyzed,
/// from its end. A copy that fails is reported and not fatal: the instance is
/// complete without it.
async fn keep_mod_list_source(game_dir: &Path, analysis: &PackAnalysis, report: PathBuf) {
    let Some(logged) = analysis.logged_mods.as_ref() else {
        return;
    };
    let relative = if logged.crash_report {
        let name = ephervex_pack::safe_file_name(&logged.source)
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| ephervex_pack::is_crash_report_name(name))
            .unwrap_or_else(|| "crash-imported.txt".to_owned());
        PathBuf::from(ephervex_pack::CRASH_REPORTS_DIR).join(name)
    } else {
        PathBuf::from(ephervex_pack::LATEST_LOG)
    };
    let destination = game_dir.join(relative);
    let copied = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut source = ephervex_pack::open_analyzed_mod_list(&report)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Published whole, so the logs view never reads half a report.
        ephervex_storage::write_atomic(
            &destination,
            ephervex_storage::AtomicWriteOptions::default(),
            |file| std::io::copy(&mut source, file).map(drop),
        )
    })
    .await;
    match copied {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!(%error, "imported the mod list without keeping its report"),
        Err(error) => warn!(%error, "imported the mod list without keeping its report"),
    }
}

/// Refuses a Minecraft release below the supported floor, offline.
///
/// Deliberately decided from the version id alone rather than by asking Mojang:
/// an import should fail on a 1.7.10 pack in a second and without a network,
/// and a version id in a numbering scheme this build does not recognize is left
/// to the launch path - which resolves it against the manifest and refuses it
/// there if it is not a real release.
fn check_supported_release(version: &str) -> CoreResult<()> {
    let id = MinecraftVersionId::parse(version);
    if id.is_supported() == Some(false) {
        return Err(MinecraftError::unsupported(version).into());
    }
    Ok(())
}

/// The name an instance gets when the caller did not choose one.
///
/// The pack's name is archive content, so it is repaired to the same rules a
/// rename enforces. A name with nothing usable left falls back to the archive's
/// file name, then to a generic one. A frontend offering a name to edit should
/// offer this one, since submitting the pack's raw name is refused when it
/// breaks those rules.
#[must_use]
pub fn suggested_instance_name(analysis: &PackAnalysis) -> String {
    let recipe = &analysis.recipe;
    sanitized_entity_name(&recipe.original_name)
        .or_else(|| {
            recipe
                .archive_name
                .as_deref()
                .and_then(sanitized_entity_name)
        })
        .unwrap_or_else(|| FALLBACK_INSTANCE_NAME.to_owned())
}

/// The instance name of a pack that supplies no usable name at all.
const FALLBACK_INSTANCE_NAME: &str = "Imported pack";

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
        let options: zip::write::FileOptions<'_, '_, ()> =
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

    #[derive(Debug)]
    struct CancelAtStage(CancellationToken);

    impl ephervex_download::ProgressSink for CancelAtStage {
        fn operation_changed(&self, operation: &ephervex_model::Operation) {
            if operation.stage.as_deref() == Some("operation-stage-applying-settings") {
                self.0.cancel();
            }
        }
    }

    #[tokio::test]
    async fn a_cancel_after_the_pack_is_laid_down_does_not_publish_the_instance() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        zip_file(
            &archive,
            &[
                (
                    "modrinth.index.json",
                    &index(r#"{"minecraft": "1.20.1", "neoforge": "47.1.106"}"#),
                ),
                ("overrides/config/a.toml", b"a" as &[u8]),
            ],
        );
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let cancel = CancellationToken::new();
        let reporter = OperationReporter::new(
            "test-import",
            true,
            std::sync::Arc::new(CancelAtStage(cancel.clone())),
        );

        let error = app
            .import_pack(&ImportRequest::new(&archive), &reporter, &cancel)
            .await
            .expect_err("a canceled import fails");

        assert!(
            matches!(error, crate::error::CoreError::Pack(PackError::Canceled)),
            "{error:?}"
        );
        assert!(
            app.list_clients().expect("list").is_empty(),
            "a canceled import leaves no instance behind"
        );
    }

    fn named_index(name_json: &str) -> Vec<u8> {
        format!(
            r#"{{
                "formatVersion": 1,
                "game": "minecraft",
                "versionId": "2.1.0",
                "name": {name_json},
                "files": [],
                "dependencies": {{"minecraft": "1.20.1", "forge": "47.4.0"}}
            }}"#
        )
        .into_bytes()
    }

    async fn import_named_pack(name_json: &str) -> (App, tempfile::TempDir, ImportedPack) {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        zip_file(
            &archive,
            &[("modrinth.index.json", &named_index(name_json))],
        );
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let imported = import(&app, &archive).await.expect("import");
        (app, base, imported)
    }

    #[tokio::test]
    async fn a_pack_name_with_control_characters_is_stored_as_a_name_a_rename_accepts() {
        let (app, _base, imported) =
            import_named_pack(r#""Evil\u001b[2J\u001b[31mPWNED\nsecond line""#).await;

        assert!(!imported.record.name.chars().any(char::is_control));
        assert!(imported.record.name.starts_with("Evil"));
        assert!(imported.record.name.ends_with("second line"));
        app.rename_client(imported.record.id, &imported.record.name)
            .expect("the stored name survives a rename of itself");
    }

    #[tokio::test]
    async fn a_pack_name_over_the_ceiling_is_cut_to_it() {
        let long = "n".repeat(crate::instance::MAX_ENTITY_NAME_CHARS + 30);
        let (app, _base, imported) = import_named_pack(&format!("\"{long}\"")).await;

        assert_eq!(
            imported.record.name.chars().count(),
            crate::instance::MAX_ENTITY_NAME_CHARS
        );
        validated_entity_name(&imported.record.name).expect("the stored name passes the rules");
        app.rename_client(imported.record.id, &imported.record.name)
            .expect("rename");
    }

    #[tokio::test]
    async fn the_suggested_name_of_an_awkward_pack_is_accepted_when_submitted_explicitly() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        zip_file(
            &archive,
            &[(
                "modrinth.index.json",
                &named_index(r#""Evil\u001b[2J\nsecond""#),
            )],
        );
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let suggested = suggested_instance_name(&app.analyze_pack(&archive).expect("analyze"));
        let imported = app
            .import_pack(
                &ImportRequest::new(&archive).named(&suggested),
                &OperationReporter::silent("test-import"),
                &CancellationToken::new(),
            )
            .await
            .expect("the suggested name is accepted as a chosen one");
        assert_eq!(imported.record.name, suggested);
    }

    #[tokio::test]
    async fn a_blank_pack_name_falls_back_to_the_archive_file_name() {
        for blank in [r#""""#, r#""  \n\t ""#] {
            let (_app, _base, imported) = import_named_pack(blank).await;
            assert_eq!(imported.record.name, "pack.mrpack", "{blank}");
        }
    }

    #[tokio::test]
    async fn an_explicit_instance_name_a_rename_would_refuse_is_refused_and_creates_nothing() {
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

        for bad in ["", "   ", "two\nlines", "x".repeat(200).as_str()] {
            let error = app
                .import_pack(
                    &ImportRequest::new(&archive).named(bad),
                    &OperationReporter::silent("test-import"),
                    &CancellationToken::new(),
                )
                .await
                .expect_err("must refuse");
            assert!(
                matches!(error, crate::error::CoreError::EntityNameInvalid { .. }),
                "{error:?}"
            );
        }
        assert!(app.list_clients().expect("list").is_empty());

        let imported = app
            .import_pack(
                &ImportRequest::new(&archive).named("  Chosen name  "),
                &OperationReporter::silent("test-import"),
                &CancellationToken::new(),
            )
            .await
            .expect("import");
        assert_eq!(imported.record.name, "Chosen name");
    }

    fn offline_pack(dir: &Path) -> PathBuf {
        let archive = dir.join("report.mrpack");
        zip_file(
            &archive,
            &[(
                "modrinth.index.json",
                &index(r#"{"minecraft": "1.20.1", "forge": "47.4.0"}"#),
            )],
        );
        archive
    }

    #[tokio::test]
    async fn an_imported_instance_keeps_the_issue_link_it_was_imported_for() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = offline_pack(dir.path());
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let imported = app
            .import_pack(
                &ImportRequest::new(&archive)
                    .with_issue_link("  https://github.com/example/cavestone/issues/42  "),
                &OperationReporter::silent("test-import"),
                &CancellationToken::new(),
            )
            .await
            .expect("import");

        let link = Some("https://github.com/example/cavestone/issues/42".to_owned());
        assert_eq!(imported.record.issue_link, link);
        assert_eq!(
            app.current_client(imported.record.id)
                .expect("read back")
                .issue_link,
            link,
            "the link is in the instance's own metadata, not only on the record returned"
        );
    }

    /// A frontend names the id to address the instance while it imports.
    #[tokio::test]
    async fn an_import_creates_the_instance_under_the_id_it_was_given() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = offline_pack(dir.path());
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let id = InstanceId::new();

        let imported = app
            .import_pack(
                &ImportRequest::new(&archive).with_instance_id(id),
                &OperationReporter::silent("test-import"),
                &CancellationToken::new(),
            )
            .await
            .expect("import");

        assert_eq!(imported.record.id, id);
        assert_eq!(app.current_client(id).expect("published").id, id);
    }

    #[tokio::test]
    async fn an_import_under_an_id_another_instance_has_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = offline_pack(dir.path());
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let first = import(&app, &archive).await.expect("import");

        let refused = app
            .import_pack(
                &ImportRequest::new(&archive).with_instance_id(first.record.id),
                &OperationReporter::silent("test-import"),
                &CancellationToken::new(),
            )
            .await;

        assert!(refused.is_err(), "a second instance under one id");
        assert_eq!(app.list_clients().expect("list").len(), 1);
    }

    #[tokio::test]
    async fn an_import_with_no_issue_link_has_none() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = offline_pack(dir.path());
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let imported = import(&app, &archive).await.expect("import");

        assert_eq!(imported.record.issue_link, None);
    }

    /// The link comes from outside, so an address the instance's own editor
    /// would refuse is refused here too, before anything is written.
    #[tokio::test]
    async fn an_issue_link_an_edit_would_refuse_is_refused_and_creates_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = offline_pack(dir.path());
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://user:secret@example.com/issues/1",
            "not an address",
        ] {
            let error = app
                .import_pack(
                    &ImportRequest::new(&archive).with_issue_link(bad),
                    &OperationReporter::silent("test-import"),
                    &CancellationToken::new(),
                )
                .await
                .expect_err("must refuse");
            assert!(
                matches!(error, crate::error::CoreError::IssueLinkInvalid { .. }),
                "{bad}: {error:?}"
            );
        }
        assert!(app.list_clients().expect("list").is_empty());
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
        // "Pack requires Fabric - reject as unsupported loader."
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
    async fn a_pack_pinning_a_forge_build_with_an_unsupported_installer_format_is_refused_before_any_instance_exists()
     {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("older-forge.mrpack");
        zip_file(
            &archive,
            &[(
                "modrinth.index.json",
                &index(r#"{"minecraft": "1.12.2", "forge": "14.23.5.2847"}"#),
            )],
        );

        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let analysis = app.analyze_pack(&archive).expect_err("analysis refuses it");
        let error = import(&app, &archive).await.expect_err("must refuse");

        for error in [analysis, error] {
            assert_eq!(
                error.message_id(),
                "error-loader-installer-format-unsupported"
            );
        }
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
