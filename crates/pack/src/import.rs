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
//! `--inspect`, and - because it is also the half that refuses an unsupported
//! loader - nothing gets created for a pack that cannot be reproduced.
//!
//! [`materialize`] is the half that writes, and it writes in the required order. It
//! stops short of applying the global Minecraft preferences: those need the
//! configuration file, which lives above this crate, and they must come *after*
//! everything here.
//!
//! # Why detection and orchestration are provider-independent
//!
//! Only [`parse_with`] knows a provider exists, and it dispatches on a
//! [`PackFormat`]. `modrinth` and `curseforge` are its two arms, and nothing in
//! the pipeline, the recipe, the installer, or the retained-source layout had
//! to learn a new vocabulary for the second.
//!
//! One thing the pipeline does have to know: a format may name files its own
//! manifest cannot describe. [`analyze`] reports those as
//! [`PendingProviderFile`] entries and [`materialize`] refuses to run while any
//! remain, so the layer that resolves them cannot be skipped by accident.

use std::path::{Path, PathBuf};

use ephervex_download::{CacheLayout, DownloadManager, OperationReporter};
use ephervex_model::{AttentionReason, ImportSourceKind, LoaderSelection};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument};

use crate::archive::{EntryDecision, PackArchive};
use crate::blocking::off_runtime;
use crate::credentials::ArtifactCredentials;
use crate::curseforge::{self, ParsedExport};
use crate::detect::PackFormat;
use crate::error::{PackError, PackResult};
use crate::install::{self, AppliedLayer, MaterializedFiles};
use crate::logged_mods::{
    CRASH_REPORTS_DIR, LATEST_LOG, LoggedModListError, LoggedModRow, LoggedMods, LoggedSide,
    ShippedJar, has_mod_list, is_crash_report, is_crash_report_name, parse_logged_mod_list,
};
use crate::modrinth::{self, ParsedPack};
use crate::policy::PackPolicy;
use crate::recipe::{
    BlockedProviderFile, DuplicateProviderFile, OptionalComponent, OptionalSelection,
    OverrideScope, PackRecipe, PackSide, PendingProviderFile, SideRequirement,
};
use crate::store::SourceStore;

/// What an archive turned out to be, before anything is written (pack analysis).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackAnalysis {
    pub format: PackFormat,
    pub recipe: PackRecipe,
    /// Files the pack declared that its own manifest cannot fully describe.
    ///
    /// Empty for a format whose manifest carries names, hashes, and addresses.
    /// A CurseForge export names its files by catalog id only, so they wait
    /// here until the provider has been asked - which is what lets an archive
    /// be identified, described, and counted with no credential and no network.
    pub pending: Vec<PendingProviderFile>,
    /// Download addresses the URL policy refused, for files that had another
    /// permitted address.
    pub rejected_urls: Vec<String>,
    /// Declared files a provider answered about but refused to serve, and
    /// that resolving against the shared cache or an explicit skip has not
    /// yet cleared. Import refuses to run while any remain and nothing has
    /// said what to do about them; see [`crate::error::PackError::ProviderFilesBlocked`].
    pub blocked: Vec<BlockedProviderFile>,
    /// Attention reasons accumulated while resolving this pack, before it
    /// becomes an instance - currently only a skipped provider file's
    /// [`AttentionReason::ManualDownloadRequired`].
    pub attention: Vec<AttentionReason>,
    /// Files a CurseForge manifest declared twice for one project, and which
    /// of each pair was kept (ADR 0031). Purely informational and never
    /// retained past this preview: import proceeds either way, so this is
    /// for a caller to show before the user commits to importing, not for
    /// the resulting instance's own attention list.
    pub duplicate_files: Vec<DuplicateProviderFile>,
    /// The hashed mod list a [`PackFormat::LoggedModList`] import rebuilds
    /// the instance from, or one a pack carries in its client overrides,
    /// which an import applies only when asked to.
    pub logged_mods: Option<LoggedMods>,
    /// Hashed jars of the mod list that no catalog recognized. Imported
    /// without them, and named on the instance as needing attention. Empty
    /// until the catalogs have been asked.
    pub missing_logged_files: Vec<LoggedModRow>,
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

    /// Who the pack credits, where its format names an author.
    #[must_use]
    pub fn author(&self) -> Option<&str> {
        self.recipe.author.as_deref()
    }

    /// Whether this pack still has files only its provider can describe.
    ///
    /// What a frontend shows an "a CurseForge API key is needed before these
    /// files can be resolved" blocker for, without knowing which provider is
    /// involved or what a credential is.
    #[must_use]
    pub fn needs_provider_resolution(&self) -> bool {
        !self.pending.is_empty()
            || self
                .logged_mods
                .as_ref()
                .is_some_and(|logged| logged.requested && !logged.resolved)
    }

    /// The optional files this side would ask about (optional-file selection).
    ///
    /// Only resolved entries appear: an unresolved one has no destination path,
    /// and a selection is made by path. A pack with pending files therefore
    /// offers its optional list once [`Self::needs_provider_resolution`] is
    /// false, and offers a count before that
    /// ([`Self::optional_file_count`]).
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
            + self
                .pending
                .iter()
                .filter(|file| file.requirement(side) == SideRequirement::Required)
                .count()
    }

    /// How many files this side would ask about before installing.
    ///
    /// Counts resolved and pending entries alike, so the number a user is shown
    /// is the pack's own and does not change when the provider answers.
    #[must_use]
    pub fn optional_file_count(&self, side: PackSide) -> usize {
        self.recipe
            .files
            .iter()
            .filter(|file| file.requirement(side).needs_decision())
            .count()
            + self
                .pending
                .iter()
                .filter(|file| file.requirement(side).needs_decision())
                .count()
    }

    /// Every file the pack declares for this side, resolved or not.
    #[must_use]
    pub fn declared_file_count(&self, side: PackSide) -> usize {
        self.recipe
            .files
            .iter()
            .filter(|file| file.requirement(side).is_installable())
            .count()
            + self
                .pending
                .iter()
                .filter(|file| file.requirement(side).is_installable())
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
        self.declared_file_count(side) > 0
    }

    /// Which override layers the archive carries.
    pub fn layers(&self) -> impl Iterator<Item = OverrideScope> + '_ {
        self.recipe.override_layers.iter().map(|layer| layer.scope)
    }
}

/// Opens an archive and works out what it is, writing nothing.
///
/// The archive is returned alongside the analysis so a caller that goes on to
/// import does not re-open the file - which would let it be swapped between the
/// decision and the extraction.
#[instrument(skip(policy), fields(archive = %path.display()))]
pub fn analyze(path: &Path, policy: &PackPolicy) -> PackResult<(PackArchive, PackAnalysis)> {
    let mut archive = PackArchive::open(path, policy.limits)?;
    let Some(format) = archive.format() else {
        return Err(PackError::FormatUnrecognized {
            path: path.to_path_buf(),
        });
    };
    let mut analysis = parse_with(format, &mut archive, policy)?;
    archive.require_extractable_layers(
        analysis
            .recipe
            .override_layers
            .iter()
            .map(|layer| layer.archive_prefix.as_str()),
    )?;
    archive.require_layers_agree(
        analysis
            .recipe
            .override_layers
            .iter()
            .map(|layer| layer.archive_prefix.as_str()),
        crate::recipe::is_mod_file_path,
    )?;
    // Only once the layers are known to be within the archive's limits.
    analysis.logged_mods = carried_mod_list(&mut archive, &analysis.recipe);
    info!(
        format = %analysis.format,
        pack = %analysis.recipe.original_name,
        minecraft = %analysis.recipe.minecraft_version,
        loader = %analysis.recipe.loader,
        files = analysis.recipe.files.len(),
        pending = analysis.pending.len(),
        "analyzed a pack archive"
    );
    Ok((archive, analysis))
}

/// The most of a crash report or log an import reads, from its end.
///
/// A long-running game writes a large `latest.log`, and the table the import
/// uses is the last one in it, so a longer file is read from its end rather
/// than refused. The limit keeps such a file from being read into memory whole.
pub const MAX_LOGGED_MOD_LIST_BYTES: u64 = 64 * 1024 * 1024;

/// Whether `path` reads as a text file, such as a crash report or log, rather
/// than an archive, judged by its first bytes.
///
/// Anything that is not plainly text is left to the archive reader, so a
/// truncated or mislabeled pack download is still reported as a broken
/// archive.
///
/// # Errors
///
/// [`PackError::Unreadable`] when the file cannot be read.
pub fn reads_as_text(path: &Path) -> PackResult<bool> {
    use std::io::Read as _;

    let unreadable = |source| PackError::Unreadable {
        path: path.to_path_buf(),
        source,
    };
    let mut head = Vec::with_capacity(TEXT_SNIFF_BYTES);
    std::fs::File::open(path)
        .map_err(unreadable)?
        .take(TEXT_SNIFF_BYTES as u64)
        .read_to_end(&mut head)
        .map_err(unreadable)?;
    Ok(!head.is_empty() && !head.starts_with(b"PK") && !head.contains(&0))
}

/// How much of a file [`reads_as_text`] looks at.
const TEXT_SNIFF_BYTES: usize = 8 * 1024;

/// Reads a crash report or log and works out the game its hashed mod list
/// describes, writing nothing.
///
/// The recipe names the game and no files:
/// the files come from looking the hashes up, which needs the network.
///
/// # Errors
///
/// [`PackError::FormatUnrecognized`] for a text with no mod list at all,
/// [`PackError::ModListHashesMissing`] for one whose mod list has no hashes,
/// and a refusal when the report does not name the exact game.
#[instrument(fields(path = %path.display()))]
pub fn analyze_logged_mod_list(path: &Path) -> PackResult<PackAnalysis> {
    let unreadable = |source| PackError::Unreadable {
        path: path.to_path_buf(),
        source,
    };
    let bytes = read_tail(path, MAX_LOGGED_MOD_LIST_BYTES).map_err(unreadable)?;
    let text = String::from_utf8_lossy(&bytes);
    let list = parse_logged_mod_list(&text).map_err(|error| match error {
        LoggedModListError::HashesMissing if !has_mod_list(&text) => {
            PackError::FormatUnrecognized {
                path: path.to_path_buf(),
            }
        }
        LoggedModListError::HashesMissing => PackError::ModListHashesMissing {
            path: path.to_path_buf(),
        },
        LoggedModListError::MinecraftVersionMissing => PackError::ModListMinecraftVersionMissing {
            path: path.to_path_buf(),
        },
        LoggedModListError::LoaderMissing => PackError::ModListLoaderMissing {
            path: path.to_path_buf(),
        },
    })?;

    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A report has no name of its own, and its file name is often just
    // `latest.log`, so the instance is named the way a new instance of the
    // same game is.
    let mut recipe = PackRecipe::for_manual_instance(
        ephervex_model::default_instance_name(
            &list.minecraft_version,
            list.loader.kind,
            list.loader.version.as_deref(),
        ),
        list.minecraft_version.clone(),
        list.loader.clone(),
    );
    recipe.source_kind = ImportSourceKind::LoggedModList;
    recipe.provider_metadata.kind = ImportSourceKind::LoggedModList;
    recipe.archive_name = Some(file_name.clone());
    info!(
        minecraft = %recipe.minecraft_version,
        loader = %recipe.loader,
        rows = list.rows.len(),
        hashed = list.hashed().count(),
        "analyzed a mod list"
    );
    let analysis = PackAnalysis {
        format: PackFormat::LoggedModList,
        recipe,
        pending: Vec::new(),
        rejected_urls: Vec::new(),
        blocked: Vec::new(),
        attention: Vec::new(),
        duplicate_files: Vec::new(),
        logged_mods: Some(LoggedMods {
            source: file_name,
            crash_report: is_crash_report(&text),
            list,
            requested: true,
            resolved: false,
            catalogs_not_asked: Vec::new(),
            shipped_jars: Vec::new(),
        }),
        missing_logged_files: Vec::new(),
    };
    Ok(analysis)
}

/// The last `limit` bytes of a file, or all of it when it is shorter.
///
/// The table an import uses is the last one in a log, so a longer log is read
/// from its end.
fn read_tail(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;

    let mut bytes = Vec::new();
    open_tail(path, limit)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// A reader over the last `limit` bytes of a file, or all of it when it is
/// shorter.
fn open_tail(path: &Path, limit: u64) -> std::io::Result<std::io::Take<std::fs::File>> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    if length > limit {
        file.seek(SeekFrom::Start(length - limit))?;
    }
    Ok(file.take(limit))
}

/// A reader over as much of a crash report or log as an import analyzes: the
/// last [`MAX_LOGGED_MOD_LIST_BYTES`] of it.
///
/// # Errors
///
/// Any error opening or seeking the file.
pub fn open_analyzed_mod_list(path: &Path) -> std::io::Result<impl std::io::Read> {
    open_tail(path, MAX_LOGGED_MOD_LIST_BYTES)
}

/// How many of a pack's newest crash reports are read looking for a list.
const CARRIED_REPORTS_TRIED: usize = 5;
/// The largest carried log or report that is read. Analysis reads it whether
/// or not the import goes on to use it, so it is held well below the entry
/// ceiling.
const MAX_CARRIED_LOG_BYTES: u64 = 256 * 1024 * 1024;

/// A hashed mod list the pack's client layers carry, if any.
///
/// `logs/latest.log` is preferred, and the newest `crash-reports/crash-*.txt`
/// is the fallback when the log has no hashed table. A later layer's copy of a
/// file wins, as it would on disk. Nothing found, or nothing readable, is not
/// a failure: importing the pack does not depend on it.
fn carried_mod_list(archive: &mut PackArchive, recipe: &PackRecipe) -> Option<LoggedMods> {
    let prefixes = client_layers_last_first(recipe);

    let found = |relative: String, bytes: Vec<u8>| {
        let text = String::from_utf8_lossy(&bytes);
        parse_logged_mod_list(&text)
            .ok()
            // A log of another game would replace the pack's versions with
            // ones it was never built for, and a dedicated server's lists none
            // of the client's own mods.
            .filter(|list| {
                list.minecraft_version == recipe.minecraft_version
                    && list.loader == recipe.loader
                    && list.side == LoggedSide::Client
            })
            .map(|list| LoggedMods {
                crash_report: is_crash_report(&text),
                source: relative,
                list,
                requested: false,
                resolved: false,
                catalogs_not_asked: Vec::new(),
                shipped_jars: Vec::new(),
            })
    };

    if let Some(logged) = read_carried(archive, &prefixes, LATEST_LOG)
        .and_then(|bytes| found(LATEST_LOG.to_owned(), bytes))
    {
        return Some(logged);
    }
    // Crash report names carry their time, so sorting them puts the newest
    // last. An older report is used when a newer one has no usable list.
    let mut reports: Vec<String> = prefixes
        .iter()
        .flat_map(|prefix| archive.layer_file_names(prefix, CRASH_REPORTS_DIR))
        .filter(|name| is_crash_report_name(name))
        .collect();
    reports.sort_unstable();
    reports.dedup();
    reports
        .into_iter()
        .rev()
        .take(CARRIED_REPORTS_TRIED)
        .find_map(|name| {
            let relative = format!("{CRASH_REPORTS_DIR}/{name}");
            let bytes = read_carried(archive, &prefixes, &relative)?;
            found(relative, bytes)
        })
}

/// The SHA-1 of every jar the pack's client layers ship directly in `mods/`,
/// counting for each name only the copy that ends up on disk.
///
/// Read only when a carried mod list is applied. Each jar is streamed from
/// the archive and hashed, never held in memory whole.
#[must_use]
pub fn shipped_jars(archive: &mut PackArchive, recipe: &PackRecipe) -> Vec<ShippedJar> {
    let mut jars = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for prefix in client_layers_last_first(recipe) {
        for name in archive.layer_file_names(&prefix, crate::managed::MODS_DIR) {
            if Path::new(&name).extension().is_none_or(|ext| ext != "jar")
                || !seen.insert(name.clone())
            {
                continue;
            }
            let relative = format!("{}/{name}", crate::managed::MODS_DIR);
            let sha1 = archive
                .hash_layer_file(&prefix, &relative)
                .unwrap_or_else(|error| {
                    debug!(%error, jar = %name, "could not hash a shipped jar");
                    None
                });
            jars.push(ShippedJar {
                file_name: name,
                sha1,
            });
        }
    }
    jars
}

/// The prefixes of the layers a client applies, the one applied last first,
/// since its copy of a file is the one that ends up on disk.
fn client_layers_last_first(recipe: &PackRecipe) -> Vec<String> {
    let mut layers: Vec<(u8, String)> = recipe
        .override_layers
        .iter()
        .filter(|layer| layer.scope.applies_to(PackSide::Client))
        .map(|layer| (layer.scope.order(), layer.archive_prefix.clone()))
        .collect();
    layers.sort_by_key(|layer| std::cmp::Reverse(layer.0));
    layers.into_iter().map(|(_, prefix)| prefix).collect()
}

/// The copy of a game-directory file the first of `prefixes` that has one
/// carries.
fn read_carried(archive: &mut PackArchive, prefixes: &[String], relative: &str) -> Option<Vec<u8>> {
    // As much as a report imported on its own is read: its end.
    let keep = usize::try_from(MAX_LOGGED_MOD_LIST_BYTES).unwrap_or(usize::MAX);
    for prefix in prefixes {
        match archive.read_layer_file_tail(prefix, relative, keep, MAX_CARRIED_LOG_BYTES) {
            Ok(Some(bytes)) => return Some(bytes),
            Ok(None) => {}
            // This layer's copy is the one that ends up on disk, so an earlier
            // layer's copy would describe another file.
            Err(error) => {
                debug!(%error, file = relative, "could not read a carried log");
                return None;
            }
        }
    }
    None
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
                pending: Vec::new(),
                rejected_urls,
                blocked: Vec::new(),
                attention: Vec::new(),
                duplicate_files: Vec::new(),
                logged_mods: None,
                missing_logged_files: Vec::new(),
            })
        }
        PackFormat::CurseForgePack => {
            // No allowlist is consulted here: a CurseForge manifest names no
            // download address at all. The addresses come back from the API,
            // and are checked against the policy by whoever asks it.
            let ParsedExport {
                recipe,
                pending,
                manifest: _,
            } = curseforge::read(archive)?;
            Ok(PackAnalysis {
                format,
                recipe,
                pending,
                rejected_urls: Vec::new(),
                blocked: Vec::new(),
                attention: Vec::new(),
                duplicate_files: Vec::new(),
                logged_mods: None,
                missing_logged_files: Vec::new(),
            })
        }
        // Not an archive, so never identified as one.
        PackFormat::PrismExport | PackFormat::LoggedModList => {
            Err(PackError::FormatUnsupported { format })
        }
    }
}

/// Fills in provider identity a stored recipe did not record.
///
/// The second dispatch point, and it exists for the same reason as
/// [`parse_with`]: only a provider's own module knows what its metadata proves.
/// A recipe written before pack files carried an identity is not re-imported
/// and is not rewritten - it is completed on the way in, deterministically,
/// from the addresses it already holds. Recorded identities are left alone.
///
/// A recipe whose source is not a provider pack has nothing to reconstruct: a
/// hand-made instance's entries were written by an installation that already
/// knew exactly what it was installing.
pub(crate) fn reconcile_identities(recipe: &mut PackRecipe) {
    match recipe.source_kind {
        ImportSourceKind::ModrinthPack => modrinth::reconcile_identities(recipe),
        ImportSourceKind::CurseForgePack
        | ImportSourceKind::PrismExport
        | ImportSourceKind::ServerPack
        | ImportSourceKind::LoggedModList
        | ImportSourceKind::Unknown
        | ImportSourceKind::Manual => {}
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
    /// Owned because extraction runs on a blocking thread, which cannot hold a
    /// borrow from the async caller.
    pub archive: PackArchive,
    pub analysis: &'a PackAnalysis,
    pub side: PackSide,
    pub selection: &'a OptionalSelection,
    pub game_dir: &'a Path,
    pub source_dir: &'a Path,
    pub manager: &'a DownloadManager,
    pub cache: &'a CacheLayout,
    /// Where a credential comes from for a provider that serves its artifacts
    /// only to an authenticated request. Consulted per file, and only for a file
    /// this import actually has to fetch.
    pub credentials: &'a dyn ArtifactCredentials,
    pub reporter: &'a OperationReporter,
    pub cancel: &'a CancellationToken,
}

/// Materializes one side of an analyzed pack (the project's order).
#[instrument(skip_all, fields(side = request.side.as_str(), game_dir = %request.game_dir.display()))]
pub async fn materialize(request: MaterializeRequest<'_>) -> PackResult<MaterializedPack> {
    let MaterializeRequest {
        mut archive,
        analysis,
        side,
        selection,
        game_dir,
        source_dir,
        manager,
        cache,
        credentials,
        reporter,
        cancel,
    } = request;
    let fetch = install::ArtifactFetch {
        manager,
        cache,
        credentials,
        reporter,
        cancel,
    };

    if cancel.is_cancelled() {
        return Err(PackError::Canceled);
    }
    require_resolved(analysis)?;

    let mut recipe = analysis.recipe.clone();
    // Recorded before anything is installed, so the recipe describes the
    // decision the import was made under even if a later step fails.
    recipe.record_selection(side, selection);

    // 1. Pack files.
    reporter.set_stage(Some("operation-stage-downloading-files".to_owned()));
    let files = install::install_files(install::FileInstall {
        recipe: &recipe,
        side,
        selection,
        game_dir,
        fetch,
    })
    .await?;

    // 2. Overrides, then side-overrides. Later layer wins.
    //
    // Silent until now, and on a large pack that is several seconds of a
    // progress bar that has stopped moving for no visible reason.
    reporter.set_stage(Some("operation-stage-extracting-overrides".to_owned()));
    let (archive, mut recipe, layers) = {
        let (cancel, game_dir) = (cancel.clone(), game_dir.to_path_buf());
        off_runtime(move || {
            let layers = install::apply_overrides(&mut archive, &recipe, side, &game_dir, &cancel)?;
            Ok((archive, recipe, layers))
        })
        .await?
    };
    let mut shipped_files: Vec<(PathBuf, OverrideScope)> = Vec::new();
    for applied in &layers {
        if let Some(layer) = recipe
            .override_layers
            .iter_mut()
            .find(|layer| layer.scope == applied.layer.scope)
        {
            layer.files = applied.files;
        }
        // The pack shipped these files rather than declaring them, or shipped
        // them over a declared file, so this is the only place the desired
        // state can learn what the pack actually places.
        shipped_files.extend(
            applied
                .shipped_files
                .iter()
                .map(|path| (path.clone(), applied.layer.scope)),
        );
    }

    // 3. Retain the source recipe, which is mandatory even for a client-only
    //    import.
    reporter.set_stage(Some("operation-stage-retaining-source".to_owned()));
    let (recipe, recipe_path) =
        retain_and_record(archive, recipe, shipped_files, side, source_dir, cancel).await?;

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

/// Refuses an analysis that still has files nobody has resolved or decided
/// about.
fn require_resolved(analysis: &PackAnalysis) -> PackResult<()> {
    if let Some(unresolved) = analysis.pending.first() {
        // Materializing an analysis that still has pending entries would lay
        // down a pack missing every file its provider was never asked about,
        // and record that as the instance's complete desired state. The caller
        // resolves first; reaching here means it did not.
        return Err(PackError::ProviderFilesUnresolved {
            remaining: analysis.pending.len(),
            first: unresolved.identity.project_id.clone().unwrap_or_default(),
        });
    }
    if let Some(logged) = analysis
        .logged_mods
        .as_ref()
        .filter(|logged| logged.requested && !logged.resolved)
    {
        // A mod list names its files only by hash, and nobody asked the
        // catalogs what they are.
        return Err(PackError::ProviderFilesUnresolved {
            remaining: logged.list.hashed().count(),
            first: logged.source.clone(),
        });
    }
    if let Some(blocked) = analysis.blocked.first() {
        // A file a provider refused is not laid down silently either: the
        // caller resolves each one first, by finding a verified copy in the
        // shared cache or by an explicit skip. Reaching here means at least
        // one is still neither.
        return Err(PackError::ProviderFilesBlocked {
            remaining: analysis.blocked.len(),
            first: blocked.identity.project_id.clone().unwrap_or_default(),
        });
    }
    Ok(())
}

/// One request to lay down a side of an analysis that came with no archive.
///
/// What [`MaterializeRequest`] is for a recipe whose files all come from the
/// catalogs, such as one rebuilt from a crash report's mod list.
#[derive(Debug)]
pub struct RecipeMaterializeRequest<'a> {
    pub analysis: &'a PackAnalysis,
    pub side: PackSide,
    pub selection: &'a OptionalSelection,
    pub game_dir: &'a Path,
    pub source_dir: &'a Path,
    pub manager: &'a DownloadManager,
    pub cache: &'a CacheLayout,
    pub credentials: &'a dyn ArtifactCredentials,
    pub reporter: &'a OperationReporter,
    pub cancel: &'a CancellationToken,
}

/// Installs one side of an archive-free analysis and retains its recipe.
///
/// # Errors
///
/// The same refusals as [`materialize`] for files still pending or blocked,
/// and any download or write failure.
#[instrument(skip_all, fields(side = request.side.as_str(), game_dir = %request.game_dir.display()))]
pub async fn materialize_recipe(
    request: RecipeMaterializeRequest<'_>,
) -> PackResult<MaterializedPack> {
    let RecipeMaterializeRequest {
        analysis,
        side,
        selection,
        game_dir,
        source_dir,
        manager,
        cache,
        credentials,
        reporter,
        cancel,
    } = request;
    if cancel.is_cancelled() {
        return Err(PackError::Canceled);
    }
    require_resolved(analysis)?;

    let mut recipe = analysis.recipe.clone();
    recipe.record_selection(side, selection);

    reporter.set_stage(Some("operation-stage-downloading-files".to_owned()));
    let files = install::install_files(install::FileInstall {
        recipe: &recipe,
        side,
        selection,
        game_dir,
        fetch: install::ArtifactFetch {
            manager,
            cache,
            credentials,
            reporter,
            cancel,
        },
    })
    .await?;

    reporter.set_stage(Some("operation-stage-retaining-source".to_owned()));
    let (recipe, recipe_path) = {
        let (cancel, source_dir) = (cancel.clone(), source_dir.to_path_buf());
        off_runtime(move || {
            if cancel.is_cancelled() {
                return Err(PackError::Canceled);
            }
            let recipe_path = SourceStore::new(&source_dir).write_recipe(&recipe)?;
            Ok((recipe, recipe_path))
        })
        .await?
    };
    info!(
        installed = files.installed.len(),
        preserved = files.skipped.len(),
        "materialized a recipe"
    );
    Ok(MaterializedPack {
        side,
        files,
        layers: Vec::new(),
        recipe,
        recipe_path,
    })
}

/// Retains the source, records what every layer placed, and writes the recipe.
///
/// On a blocking thread, since retaining copies whole override layers.
async fn retain_and_record(
    mut archive: PackArchive,
    mut recipe: PackRecipe,
    mut shipped_files: Vec<(PathBuf, OverrideScope)>,
    side: PackSide,
    source_dir: &Path,
    cancel: &CancellationToken,
) -> PackResult<(PackRecipe, PathBuf)> {
    let (cancel, source_dir) = (cancel.clone(), source_dir.to_path_buf());
    off_runtime(move || {
        let store = SourceStore::new(&source_dir);
        shipped_files.extend(retain_source(
            &mut archive,
            &mut recipe,
            side,
            &store,
            &cancel,
        )?);
        // Recorded once every layer is known: what a shipped path means depends
        // on all the layers that place it, and on the declared file at the same
        // path.
        recipe.record_layer_files(&shipped_files)?;
        // A cancel that arrived while the layers were copying stops short of
        // writing the recipe.
        if cancel.is_cancelled() {
            return Err(PackError::Canceled);
        }
        let recipe_path = store.write_recipe(&recipe)?;
        Ok((recipe, recipe_path))
    })
    .await
}

/// Keeps everything a server created later will need.
///
/// The provider's manifest is kept byte for byte, and every override layer the
/// *other* side needs is copied out of the archive before it goes away. The
/// layer this side just extracted into the game directory is not duplicated:
/// A modpack's overrides are often the bulk of its bytes, so the layer already
/// installed for this side is not duplicated.
///
/// Returns the mod jars, declared files, and resource, shader, and data pack
/// files the retained layers place, for the recipe to record together with the
/// applied layers' files.
fn retain_source(
    archive: &mut PackArchive,
    recipe: &mut PackRecipe,
    side: PackSide,
    store: &SourceStore,
    cancel: &CancellationToken,
) -> PackResult<Vec<(PathBuf, OverrideScope)>> {
    let kind = recipe.source_kind;
    if let Some(entry) = manifest_entry_for(kind) {
        // The bytes analysis already parsed, not a second read of the entry.
        let bytes = archive.read_manifest(entry)?;
        let relative = store.retain_manifest(kind, entry, &bytes)?;
        recipe.provider_metadata.manifest_file = Some(relative);
    }

    let already_applied = side.override_scope();
    // Collected while the layers are borrowed.
    let mut retained_files: Vec<(OverrideScope, Vec<PathBuf>)> = Vec::new();
    let filter = recipe.layer_path_filter();
    for layer in &mut recipe.override_layers {
        if layer.scope == already_applied {
            // Already in the game directory, so it does not need to be
            // duplicated. The recipe records that it was not retained so a
            // later plan can say so rather than assume.
            layer.retained_at = None;
            continue;
        }
        let (absolute, relative) = store.layer_dir(kind, &layer.archive_prefix);
        // The same record the applied layers get. A client import retains the
        // server layer without applying it, and a server built from this
        // instance loads whatever that layer places.
        let mut shipped_files = Vec::new();
        let report = archive.extract_layer_filtered(
            &layer.archive_prefix,
            &absolute,
            &mut |path| {
                if filter.selects(path) {
                    shipped_files.push(path.to_path_buf());
                }
                EntryDecision::Extract
            },
            cancel,
        )?;
        layer.files = report.files;
        layer.retained_at = Some(relative);
        retained_files.push((layer.scope, shipped_files));
    }
    Ok(retained_files
        .into_iter()
        .flat_map(|(scope, paths)| paths.into_iter().map(move |path| (path, scope)))
        .collect())
}

/// The manifest file name one provider's retained state keeps.
const fn manifest_entry_for(kind: ImportSourceKind) -> Option<&'static str> {
    match kind {
        ImportSourceKind::ModrinthPack => Some(crate::detect::MODRINTH_INDEX),
        ImportSourceKind::CurseForgePack => Some(crate::detect::CURSEFORGE_MANIFEST),
        ImportSourceKind::PrismExport => Some(crate::detect::PRISM_PACK),
        ImportSourceKind::ServerPack
        | ImportSourceKind::LoggedModList
        | ImportSourceKind::Unknown
        | ImportSourceKind::Manual => None,
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

    #[test]
    fn a_carried_mod_list_nobody_asked_for_does_not_hold_up_materializing() {
        let report = "---- Minecraft Crash Report ----\n\tMinecraft Version: 1.21.1\n\tNeoForge: 21.1.77\n\tMod List: \n\t\ta.jar |A |a |1.0 |Manifest: NOSIGNATURE|SHA1: da39a3ee5e6b4b0d3255bfef95601890afd80709|Murmur2: 1\n";
        let list = parse_logged_mod_list(report).expect("hashed list");
        let logged = |requested| LoggedMods {
            source: "logs/latest.log".to_owned(),
            list: list.clone(),
            crash_report: true,
            requested,
            resolved: false,
            catalogs_not_asked: Vec::new(),
            shipped_jars: Vec::new(),
        };
        let mut analysis = PackAnalysis {
            format: PackFormat::ModrinthPack,
            recipe: PackRecipe::for_manual_instance("Pack", "1.21.1", list.loader.clone()),
            pending: Vec::new(),
            rejected_urls: Vec::new(),
            blocked: Vec::new(),
            attention: Vec::new(),
            duplicate_files: Vec::new(),
            logged_mods: Some(logged(false)),
            missing_logged_files: Vec::new(),
        };

        assert!(require_resolved(&analysis).is_ok());
        analysis.logged_mods = Some(logged(true));
        assert!(matches!(
            require_resolved(&analysis),
            Err(PackError::ProviderFilesUnresolved { .. })
        ));
    }

    #[test]
    fn a_long_log_is_read_from_its_end() {
        let directory = tempfile::tempdir().expect("temp dir");
        let log = directory.path().join("latest.log");
        std::fs::write(&log, b"startup lines\nthe crash report").expect("write");

        assert_eq!(read_tail(&log, 16).expect("read"), b"the crash report");
        assert_eq!(
            read_tail(&log, 1024).expect("read"),
            b"startup lines\nthe crash report"
        );
    }
    use crate::credentials::OpenArtifacts;

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

    /// Like `zip_file`, with the entries named in `encrypted` written
    /// AES-encrypted so that opening them without a password fails.
    fn zip_file_with_encrypted(path: &Path, entries: &[(&str, &[u8])], encrypted: &[&str]) {
        let file = std::fs::File::create(path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        let plain: zip::write::FileOptions<'_, '_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            let options = if encrypted.contains(name) {
                plain.with_aes_encryption(zip::AesMode::Aes256, "secret")
            } else {
                plain
            };
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
    fn analysis_refuses_an_entry_name_that_extraction_would_refuse_outside_any_layer() {
        for unsafe_name in [
            "docs\\readme.txt",
            "Pack: v2/readme.txt",
            "notes/../../escape.txt",
            "/absolute.txt",
            ".launcher/instance.json",
        ] {
            let dir = tempfile::tempdir().expect("temp dir");
            let archive = dir.path().join("pack.mrpack");
            zip_file(
                &archive,
                &[
                    ("modrinth.index.json", INDEX),
                    ("overrides/config/a.toml", b"a"),
                    (unsafe_name, b"x"),
                ],
            );

            let error = analyze(&archive, &PackPolicy::modrinth())
                .map(|_| ())
                .expect_err(unsafe_name);
            assert!(
                matches!(
                    error,
                    PackError::EntryPathUnsafe { .. } | PackError::EntryReserved { .. }
                ),
                "{unsafe_name} was refused with {error:?}"
            );
            assert!(ephervex_i18n::has(error.message_id()));
        }
    }

    #[test]
    fn analysis_refuses_an_encrypted_entry_outside_any_layer() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        zip_file_with_encrypted(
            &archive,
            &[
                ("modrinth.index.json", INDEX),
                ("overrides/config/a.toml", b"a"),
                ("notes/readme.txt", b"locked"),
            ],
            &["notes/readme.txt"],
        );

        let error = analyze(&archive, &PackPolicy::modrinth())
            .map(|_| ())
            .expect_err("refused");

        assert!(matches!(error, PackError::ArchiveInvalid { .. }));
        assert!(ephervex_i18n::has(error.message_id()));
    }

    #[test]
    fn analysis_refuses_an_unsafe_entry_name_in_a_curseforge_export() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("repro.zip");
        zip_file(
            &archive,
            &[
                ("manifest.json", &curseforge_manifest("")),
                ("docs\\readme.txt", b"x" as &[u8]),
            ],
        );

        let error = analyze(&archive, &PackPolicy::curseforge())
            .map(|_| ())
            .expect_err("refused");
        assert!(matches!(error, PackError::EntryPathUnsafe { .. }));
    }

    #[test]
    fn analysis_refuses_a_reserved_name_that_only_appears_beneath_a_modrinth_override_layer() {
        for root in ["", "./", "Wrapper/"] {
            for layer in ["overrides", "client-overrides", "server-overrides"] {
                for reserved in [".launcher/instance.json", ".LAUNCHER/x"] {
                    let dir = tempfile::tempdir().expect("temp dir");
                    let archive = dir.path().join("pack.mrpack");
                    let name = under(root, &format!("{layer}/{reserved}"));
                    zip_file(
                        &archive,
                        &[
                            (&under(root, "modrinth.index.json"), INDEX),
                            (&under(root, "overrides/config/a.toml"), b"a"),
                            (&under(root, "client-overrides/b.txt"), b"b"),
                            (&under(root, "server-overrides/c.txt"), b"c"),
                            (&name, b"x"),
                        ],
                    );

                    let error = analyze(&archive, &PackPolicy::modrinth())
                        .map(|_| ())
                        .expect_err(&name);
                    assert!(
                        matches!(error, PackError::EntryReserved { .. }),
                        "{name} was refused with {error:?}"
                    );
                    assert!(ephervex_i18n::has(error.message_id()));
                    assert_eq!(
                        std::fs::read_dir(dir.path()).expect("list").count(),
                        1,
                        "{name}: analysis writes nothing beside the archive"
                    );
                }
            }
        }
    }

    fn analyze_layered_jars(entries: &[(&str, &[u8])]) -> (tempfile::TempDir, PackResult<()>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        let mut all: Vec<(&str, &[u8])> = vec![("modrinth.index.json", INDEX)];
        all.extend_from_slice(entries);
        zip_file(&archive, &all);
        let result = analyze(&archive, &PackPolicy::modrinth()).map(|_| ());
        (dir, result)
    }

    #[test]
    fn analysis_refuses_a_mod_whose_bytes_differ_between_override_layers() {
        let layer_pairs = [
            ("overrides", "client-overrides"),
            ("overrides", "server-overrides"),
            ("client-overrides", "server-overrides"),
        ];
        for (first, second) in layer_pairs {
            let (dir, result) = analyze_layered_jars(&[
                (&format!("{first}/mods/x.jar"), b"first"),
                (&format!("{second}/mods/x.jar"), b"second"),
            ]);
            let error = result.expect_err(first);
            assert!(
                matches!(error, PackError::OverrideLayersDisagree { ref path } if path == Path::new("mods/x.jar")),
                "{first} and {second} were refused with {error:?}"
            );
            assert!(ephervex_i18n::has(error.message_id()));
            assert_eq!(
                std::fs::read_dir(dir.path()).expect("list").count(),
                1,
                "analysis writes nothing beside the archive"
            );
        }
    }

    #[test]
    fn analysis_refuses_a_mod_that_differs_in_only_one_of_three_override_layers() {
        let (_dir, result) = analyze_layered_jars(&[
            ("overrides/mods/x.jar", b"same"),
            ("client-overrides/mods/x.jar", b"same"),
            ("server-overrides/mods/x.jar", b"other"),
        ]);
        assert!(matches!(
            result.expect_err("refused"),
            PackError::OverrideLayersDisagree { .. }
        ));
    }

    #[test]
    fn analysis_accepts_a_mod_with_identical_bytes_in_every_override_layer() {
        let (_dir, result) = analyze_layered_jars(&[
            ("overrides/mods/x.jar", b"same"),
            ("client-overrides/mods/x.jar", b"same"),
            ("server-overrides/mods/x.jar", b"same"),
        ]);
        result.expect("identical bytes describe every side truthfully");
    }

    #[test]
    fn analysis_accepts_different_mods_per_override_layer_and_differing_untracked_files() {
        let (_dir, result) = analyze_layered_jars(&[
            ("overrides/mods/a.jar", b"a"),
            ("client-overrides/mods/b.jar", b"b"),
            ("overrides/config/x.toml", b"one"),
            ("client-overrides/config/x.toml", b"two"),
        ]);
        result.expect("only a shared mod path needs one set of bytes");
    }

    #[test]
    fn analysis_refuses_a_reserved_name_that_only_appears_beneath_a_curseforge_override_layer() {
        for layer in ["overrides", "custom-overrides"] {
            let dir = tempfile::tempdir().expect("temp dir");
            let archive = dir.path().join("repro.zip");
            let manifest = String::from_utf8(curseforge_manifest(""))
                .expect("utf-8")
                .replace(
                    "\"overrides\": \"overrides\"",
                    &format!("\"overrides\": \"{layer}\""),
                )
                .into_bytes();
            let name = format!("{layer}/.launcher/instance.json");
            zip_file(
                &archive,
                &[
                    ("manifest.json", &manifest),
                    (&format!("{layer}/config/a.toml"), b"a"),
                    (&name, b"x"),
                ],
            );

            let error = analyze(&archive, &PackPolicy::curseforge())
                .map(|_| ())
                .expect_err(&name);
            assert!(
                matches!(error, PackError::EntryReserved { .. }),
                "{name} was refused with {error:?}"
            );
        }
    }

    #[test]
    fn a_recognized_but_unsupported_format_is_not_reported_as_unrecognized() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("instance.zip");
        zip_file(
            &archive,
            &[
                ("mmc-pack.json", br#"{"formatVersion":1,"components":[]}"#),
                ("instance.cfg", b"name=Repro\n"),
            ],
        );

        let error = analyze(&archive, &PackPolicy::default()).expect_err("unsupported format");
        assert!(matches!(
            error,
            PackError::FormatUnsupported {
                format: PackFormat::PrismExport
            }
        ));
        assert_eq!(error.message_id(), "error-pack-format-unsupported");
    }

    /// A minimal but complete CurseForge export manifest.
    fn curseforge_manifest(files: &str) -> Vec<u8> {
        format!(
            r#"{{
                "minecraft": {{
                    "version": "1.20.1",
                    "modLoaders": [{{"id": "forge-47.4.10", "primary": true}}]
                }},
                "manifestType": "minecraftModpack",
                "manifestVersion": 1,
                "name": "Repro Pack",
                "version": "3.1.0",
                "author": "A Reporter",
                "files": [{files}],
                "overrides": "overrides"
            }}"#
        )
        .into_bytes()
    }

    #[test]
    fn a_curseforge_export_is_described_completely_without_a_credential_or_a_network() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("repro.zip");
        zip_file(
            &archive,
            &[
                (
                    "manifest.json",
                    &curseforge_manifest(
                        r#"{"projectID": 238222, "fileID": 4593548, "required": true},
                           {"projectID": 238223, "fileID": 4593549, "required": true},
                           {"projectID": 238224, "fileID": 4593550, "required": false}"#,
                    ),
                ),
                ("overrides/config/a.toml", b"a" as &[u8]),
            ],
        );

        let (_, analysis) = analyze(&archive, &PackPolicy::curseforge()).expect("analyze");
        assert_eq!(analysis.format, PackFormat::CurseForgePack);
        assert_eq!(analysis.name(), "Repro Pack");
        assert_eq!(analysis.minecraft_version(), "1.20.1");
        assert_eq!(analysis.loader().to_string(), "Forge 47.4.10");
        assert_eq!(analysis.author(), Some("A Reporter"));
        assert_eq!(analysis.recipe.original_version.as_deref(), Some("3.1.0"));

        // Everything a wizard needs to describe the pack, counted from the
        // manifest alone.
        assert_eq!(analysis.required_file_count(PackSide::Client), 2);
        assert_eq!(analysis.optional_file_count(PackSide::Client), 1);
        assert_eq!(analysis.declared_file_count(PackSide::Client), 3);
        assert!(analysis.needs_provider_resolution());

        // ...and the archive's own override layer.
        let layers: Vec<OverrideScope> = analysis.layers().collect();
        assert_eq!(layers, vec![OverrideScope::Common]);
    }

    #[test]
    fn a_curseforge_export_with_no_overrides_directory_records_no_layer() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("repro.zip");
        zip_file(&archive, &[("manifest.json", &curseforge_manifest(""))]);

        let (_, analysis) = analyze(&archive, &PackPolicy::curseforge()).expect("analyze");
        assert!(
            analysis.recipe.override_layers.is_empty(),
            "the manifest names the directory; the archive decides whether it is there"
        );
    }

    #[tokio::test]
    async fn materializing_a_pack_whose_provider_files_are_unresolved_is_refused() {
        // Laying this down would install the overrides and record an empty
        // desired state as though the pack had asked for nothing.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive_path = dir.path().join("repro.zip");
        zip_file(
            &archive_path,
            &[(
                "manifest.json",
                &curseforge_manifest(
                    r#"{"projectID": 238222, "fileID": 4593548, "required": true}"#,
                ),
            )],
        );

        let (archive, analysis) =
            analyze(&archive_path, &PackPolicy::curseforge()).expect("analyze");
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let error = materialize(MaterializeRequest {
            archive,
            analysis: &analysis,
            side: PackSide::Client,
            selection: &OptionalSelection::none(),
            game_dir: &dir.path().join("instance/minecraft"),
            source_dir: &dir.path().join("instance/.launcher/source"),
            manager: &manager,
            cache: &CacheLayout::new(dir.path().join("cache")),
            credentials: &OpenArtifacts,
            reporter: &OperationReporter::silent("test-import"),
            cancel: &CancellationToken::new(),
        })
        .await
        .expect_err("must refuse");

        assert!(matches!(
            error,
            PackError::ProviderFilesUnresolved { remaining: 1, .. }
        ));
        assert!(
            !dir.path().join("instance").exists(),
            "nothing was written for a pack that was refused"
        );
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

        let (archive, analysis) = analyze(&archive_path, &PackPolicy::modrinth()).expect("analyze");
        let game = dir.path().join("instance/minecraft");
        let source = dir.path().join("instance/.launcher/source");

        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let materialized = materialize(MaterializeRequest {
            archive,
            analysis: &analysis,
            side: PackSide::Client,
            selection: &OptionalSelection::none(),
            game_dir: &game,
            source_dir: &source,
            manager: &manager,
            cache: &CacheLayout::new(dir.path().join("cache")),
            credentials: &OpenArtifacts,
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

    fn under(root: &str, name: &str) -> String {
        format!("{root}{name}")
    }

    async fn materialize_client(
        dir: &Path,
        archive_path: &Path,
        policy: &PackPolicy,
    ) -> PackResult<(MaterializedPack, PathBuf, PathBuf)> {
        let (archive, analysis) = analyze(archive_path, policy)?;
        let game = dir.join("instance/minecraft");
        let source = dir.join("instance/.launcher/source");
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let materialized = materialize(MaterializeRequest {
            archive,
            analysis: &analysis,
            side: PackSide::Client,
            selection: &OptionalSelection::none(),
            game_dir: &game,
            source_dir: &source,
            manager: &manager,
            cache: &CacheLayout::new(dir.join("cache")),
            credentials: &OpenArtifacts,
            reporter: &OperationReporter::silent("test-import"),
            cancel: &CancellationToken::new(),
        })
        .await?;
        Ok((materialized, game, source))
    }

    #[tokio::test]
    async fn a_modrinth_pack_under_a_wrapper_or_a_dot_prefix_imports_from_the_root_detection_chose()
    {
        for root in ["", "./", "Wrapper/", "./Wrapper/"] {
            let dir = tempfile::tempdir().expect("temp dir");
            let archive_path = dir.path().join("pack.mrpack");
            zip_file(
                &archive_path,
                &[
                    (&under(root, "modrinth.index.json"), INDEX),
                    (&under(root, "overrides/config/common.toml"), b"common"),
                    (
                        &under(root, "client-overrides/options.txt"),
                        b"guiScale:1\n",
                    ),
                    (
                        &under(root, "server-overrides/server.properties"),
                        b"motd=t\n",
                    ),
                ],
            );

            let (materialized, game, source) =
                materialize_client(dir.path(), &archive_path, &PackPolicy::modrinth())
                    .await
                    .unwrap_or_else(|error| panic!("{root:?}: {error}"));

            assert_eq!(
                std::fs::read_to_string(game.join("config/common.toml")).expect("common"),
                "common",
                "{root:?}"
            );
            assert!(game.join("options.txt").is_file(), "{root:?}");
            assert_eq!(
                std::fs::read(source.join("modrinth/modrinth.index.json")).expect("manifest"),
                INDEX,
                "{root:?}: the manifest is retained byte for byte"
            );
            assert!(
                source
                    .join("modrinth/server-overrides/server.properties")
                    .is_file(),
                "{root:?}: a retained layer keeps its logical name, not the wrapper's"
            );
            let prefixes: Vec<&str> = materialized
                .recipe
                .override_layers
                .iter()
                .map(|layer| layer.archive_prefix.as_str())
                .collect();
            assert_eq!(
                prefixes,
                vec!["overrides", "client-overrides", "server-overrides"],
                "{root:?}: the recipe records layer names without the wrapper"
            );
        }
    }

    #[tokio::test]
    async fn a_curseforge_export_under_a_wrapper_or_a_dot_prefix_imports_from_the_root_detection_chose()
     {
        for root in ["", "./", "Wrapper/", "./Wrapper/"] {
            let dir = tempfile::tempdir().expect("temp dir");
            let archive_path = dir.path().join("export.zip");
            let manifest = curseforge_manifest("");
            zip_file(
                &archive_path,
                &[
                    (&under(root, "manifest.json"), &manifest),
                    (&under(root, "overrides/config/a.toml"), b"a"),
                ],
            );

            let (materialized, game, source) =
                materialize_client(dir.path(), &archive_path, &PackPolicy::curseforge())
                    .await
                    .unwrap_or_else(|error| panic!("{root:?}: {error}"));

            assert_eq!(
                std::fs::read_to_string(game.join("config/a.toml")).expect("override"),
                "a",
                "{root:?}"
            );
            assert_eq!(
                std::fs::read(source.join("curseforge/manifest.json")).expect("manifest"),
                manifest,
                "{root:?}"
            );
            assert_eq!(materialized.layers.len(), 1, "{root:?}");
        }
    }

    #[tokio::test]
    async fn an_import_reads_and_charges_the_manifest_once() {
        // Five charges when the manifest is read once: the manifest, the common
        // file (applied, then retained for a server), the client file, and the
        // retained server file. A second manifest read would be a sixth.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive_path = dir.path().join("pack.mrpack");
        zip_file(
            &archive_path,
            &[
                ("modrinth.index.json", INDEX),
                ("overrides/config/common.toml", b"common"),
                ("client-overrides/options.txt", b"guiScale:1\n"),
                ("server-overrides/server.properties", b"motd=t\n"),
            ],
        );
        let mut policy = PackPolicy::modrinth();
        policy.limits.max_entries = 5;

        materialize_client(dir.path(), &archive_path, &policy)
            .await
            .expect("the manifest is charged once");
    }

    /// Cancels `cancel` the moment the operation enters `stage`.
    #[derive(Debug)]
    struct CancelAtStage {
        stage: &'static str,
        cancel: CancellationToken,
    }

    impl ephervex_download::ProgressSink for CancelAtStage {
        fn operation_changed(&self, operation: &ephervex_model::Operation) {
            if operation.stage.as_deref() == Some(self.stage) {
                self.cancel.cancel();
            }
        }
    }

    fn layered_pack(dir: &Path) -> PathBuf {
        let archive_path = dir.join("pack.mrpack");
        zip_file(
            &archive_path,
            &[
                ("modrinth.index.json", INDEX),
                ("overrides/config/common.toml", b"common"),
                ("client-overrides/options.txt", b"guiScale:1\n"),
                ("server-overrides/server.properties", b"motd=test\n"),
            ],
        );
        archive_path
    }

    async fn materialize_canceling_at(
        dir: &Path,
        stage: &'static str,
    ) -> (PackResult<MaterializedPack>, PathBuf) {
        let (archive, analysis) =
            analyze(&layered_pack(dir), &PackPolicy::modrinth()).expect("analyze");
        let source = dir.join("instance/.launcher/source");
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let cancel = CancellationToken::new();
        let sink = std::sync::Arc::new(CancelAtStage {
            stage,
            cancel: cancel.clone(),
        });
        let outcome = materialize(MaterializeRequest {
            archive,
            analysis: &analysis,
            side: PackSide::Client,
            selection: &OptionalSelection::none(),
            game_dir: &dir.join("instance/minecraft"),
            source_dir: &source,
            manager: &manager,
            cache: &CacheLayout::new(dir.join("cache")),
            credentials: &OpenArtifacts,
            reporter: &OperationReporter::new("test-import", true, sink),
            cancel: &cancel,
        })
        .await;
        (outcome, source)
    }

    #[tokio::test]
    async fn a_cancel_during_override_extraction_fails_the_import_and_retains_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (outcome, source) =
            materialize_canceling_at(dir.path(), "operation-stage-extracting-overrides").await;

        assert!(matches!(outcome, Err(PackError::Canceled)), "{outcome:?}");
        assert!(!has_retained_recipe(&source));
        assert!(!dir.path().join("instance/minecraft/options.txt").exists());
    }

    #[tokio::test]
    async fn a_cancel_during_source_retention_fails_the_import_without_a_recipe() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (outcome, source) =
            materialize_canceling_at(dir.path(), "operation-stage-retaining-source").await;

        assert!(matches!(outcome, Err(PackError::Canceled)), "{outcome:?}");
        assert!(!has_retained_recipe(&source));
    }

    #[tokio::test]
    async fn override_extraction_leaves_the_runtime_free_to_run_other_tasks() {
        // A current-thread runtime has one worker, so another task can only run
        // while this one is suspended. Extraction on the worker itself would
        // finish before that task was ever polled.
        #[derive(Debug)]
        struct RecordAtStage {
            stage: &'static str,
            flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
            seen: std::sync::Arc<std::sync::atomic::AtomicBool>,
        }
        impl ephervex_download::ProgressSink for RecordAtStage {
            fn operation_changed(&self, operation: &ephervex_model::Operation) {
                use std::sync::atomic::Ordering;
                if operation.stage.as_deref() == Some(self.stage) {
                    self.seen
                        .store(self.flag.load(Ordering::SeqCst), Ordering::SeqCst);
                }
            }
        }

        let dir = tempfile::tempdir().expect("temp dir");
        let (archive, analysis) =
            analyze(&layered_pack(dir.path()), &PackPolicy::modrinth()).expect("analyze");
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let other_task = tokio::spawn({
            let flag = std::sync::Arc::clone(&flag);
            async move { flag.store(true, std::sync::atomic::Ordering::SeqCst) }
        });
        let sink = std::sync::Arc::new(RecordAtStage {
            stage: "operation-stage-retaining-source",
            flag,
            seen: std::sync::Arc::clone(&seen),
        });

        materialize(MaterializeRequest {
            archive,
            analysis: &analysis,
            side: PackSide::Client,
            selection: &OptionalSelection::none(),
            game_dir: &dir.path().join("instance/minecraft"),
            source_dir: &dir.path().join("instance/.launcher/source"),
            manager: &manager,
            cache: &CacheLayout::new(dir.path().join("cache")),
            credentials: &OpenArtifacts,
            reporter: &OperationReporter::new("test-import", true, sink),
            cancel: &CancellationToken::new(),
        })
        .await
        .expect("materialize");
        other_task.await.expect("the other task finished");

        assert!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            "another task ran while the overrides were being extracted"
        );
    }
}
