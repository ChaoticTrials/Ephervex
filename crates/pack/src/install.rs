//! Turning a recipe into files on disk.
//!
//! Provider-neutral by construction: everything here works from
//! [`PackRecipe`], so the Modrinth importer, the CurseForge importer, and
//! "Create Server" all share one materialization path. A provider's job ends
//! when it has produced a recipe.
//!
//! # The order is the specification
//!
//! ```text
//! pack files -> overrides -> <side>-overrides -> global managed preferences
//! ```
//!
//! Each step overwrites the one before it. The last
//! step is deliberately *not* here - applying global preferences needs the
//! configuration file, which lives above this crate - but the ordering
//! constraint it creates is: this module must finish before the preferences are
//! applied, or a managed GUI scale would be overwritten by the pack's own
//! `options.txt`.
//!
//! # Downloads land in the shared cache first
//!
//! A pack file is fetched into the launcher-level `downloads/` store, addressed
//! by the hash the pack published, and then copied into the instance. Two
//! reasons, both of which matter for a launcher whose defining default is a
//! disposable instance:
//!
//! * importing the same pack twice - which is what bisecting a bug looks like -
//!   costs no network at all the second time;
//! * the download manager's atomicity invariant means a file at a cache path
//!   has already been verified, so the copy into the instance is a copy of
//!   something known good rather than of something hopefully good.
//!
//! The instance still owns its own copy of every mod: `mods/`, `config/`, and
//! overrides are instance-local.
//!
//! # What a recipe entry is enough for, and what it is not
//!
//! An entry records the exact addresses, digests, and size of its artifact, plus
//! the provider identity it came from. That is everything needed to *identify
//! and verify* the file, which is why nothing here consults a provider to find
//! out what an instance is supposed to contain, and why a materialization can
//! run with no network at all when the shared cache still holds what it needs.
//!
//! It is not everything needed to *serve* the bytes. A provider may hand out its
//! artifacts only to an authenticated request, so an entry whose verified copy
//! the cache budget has since collected needs that provider's credential back
//! before it can be fetched again. The two halves stay separate here: the cache
//! is probed first, a hit is installed with no credential and no request, and
//! only a genuine fetch asks [`ArtifactCredentials`] - per file, from the
//! provider identity that file's own entry records.
//!
//! A cache that cannot be *read* belongs to neither half. It is a local storage
//! failure and is reported as one: recovering from it by fetching would ask for
//! a credential on the strength of a filesystem fault, for an artifact no
//! request was ever going to be made for.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ephervex_download::{
    ArtifactSource, ArtifactStep, AuthenticatedSource, CacheLayout, ContentStore, DownloadError,
    DownloadManager, DownloadOutcome, DownloadRequest, FileExtension, OperationReporter, Url,
    UrlPolicyError, file_matches_hashes,
};
use ephervex_model::Hash;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::archive::{EntryDecision, ExtractionLimits, ExtractionReport, PackArchive};
use crate::credentials::ArtifactCredentials;
use crate::error::{PackError, PackResult};
use crate::path::{contained_join, is_reserved, safe_relative_path};
use crate::recipe::{
    OptionalSelection, OverrideLayer, PackFileRecipe, PackRecipe, PackSide,
    ServerReconstructionPlan, UnknownServerFiles, is_mod_file_path,
};
use crate::store::SourceStore;

/// What materializing one side of a pack produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializedFiles {
    /// Logical paths written into the game directory.
    pub installed: Vec<PathBuf>,
    /// Files the recipe carries that this side does not take: server-only mods
    /// during a client import, and optional files nobody selected. Reported
    /// rather than dropped silently, because "where is my mod" has an answer
    /// and it is this list.
    pub skipped: Vec<PathBuf>,
    /// Files that were already in the shared cache from an earlier import.
    pub reused_from_cache: usize,
    pub bytes: u64,
}

/// Everything one operation needs to put a verified artifact in the shared
/// cache.
///
/// Grouped because these five always travel together and are meaningless apart:
/// where artifacts go, who fetches them, which provider credentials are
/// available should one turn out to be needed, and how progress and
/// cancellation are reported. Passing them as one value is also what keeps a
/// new one from growing every signature between here and the download manager.
#[derive(Debug, Clone, Copy)]
pub struct ArtifactFetch<'a> {
    pub manager: &'a DownloadManager,
    pub cache: &'a CacheLayout,
    /// Where a credential comes from when an artifact's provider requires one
    /// to serve it.
    ///
    /// Asked per file and only for a file that is about to be fetched. A
    /// verified copy already in the shared cache is installed without consulting
    /// this at all, which is the whole reason an installed instance keeps
    /// working on a machine that has configured no provider. See
    /// [`ArtifactCredentials`].
    pub credentials: &'a dyn ArtifactCredentials,
    pub reporter: &'a OperationReporter,
    pub cancel: &'a CancellationToken,
}

/// One request to install a side's pack files.
///
/// A struct rather than a long argument list, matching `ephervex-loaders`'
/// install requests: every field is a collaborator the caller owns, and naming
/// them at the call site is what keeps `game_dir` and `cache` from being passed
/// the wrong way round.
#[derive(Debug, Clone, Copy)]
pub struct FileInstall<'a> {
    pub recipe: &'a PackRecipe,
    pub side: PackSide,
    pub selection: &'a OptionalSelection,
    /// The directory the game runs in. Pack file paths are relative to it.
    pub game_dir: &'a Path,
    pub fetch: ArtifactFetch<'a>,
}

/// One linked-server reconstruction request.
#[derive(Debug, Clone, Copy)]
pub struct ServerMaterializeRequest<'a> {
    pub recipe: &'a PackRecipe,
    /// What to do with entries whose server side the pack never declared.
    pub unknown_server_files: &'a UnknownServerFiles,
    pub game_dir: &'a Path,
    /// The client's own game directory, which is where a file the recipe
    /// records no address for has to come from.
    pub client_game_dir: &'a Path,
    pub client_source_dir: &'a Path,
    pub server_source_dir: &'a Path,
    pub fetch: ArtifactFetch<'a>,
}

/// What reconstructing a linked server from retained desired state produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedServerRecipe {
    pub files: MaterializedFiles,
    pub layers: Vec<AppliedLayer>,
    pub recipe_path: PathBuf,
    pub unconfirmed_unknown: Vec<PathBuf>,
}

/// Downloads and installs the pack files one side needs.
///
/// Fetching is [`fetch_all`]'s job, so this and managed-mod installation share
/// one batching and mirror-fallback policy rather than two that can drift.
#[instrument(skip_all, fields(side = request.side.as_str(), game_dir = %request.game_dir.display()))]
pub async fn install_files(request: FileInstall<'_>) -> PackResult<MaterializedFiles> {
    let FileInstall {
        recipe,
        side,
        selection,
        game_dir,
        fetch,
    } = request;

    let wanted = recipe.files_for(side, selection);
    install_selected_files(&recipe.files, &wanted, game_dir, None, fetch).await
}

/// Puts one recorded file back where the recipe says it belongs.
///
/// For a file the recipe declares and the game directory no longer has: a mod
/// somebody deleted to bisect a crash, and now wants back. Nothing about the
/// recipe changes, because nothing about what the instance is supposed to
/// contain changed; only the directory is brought back into line with it.
///
/// Deliberately not a mutation. A mutation decides what an instance should
/// contain and needs a provider identity to resolve; this needs neither,
/// which is what makes it work for a pack file served from a mirror that no
/// catalog can name.
#[instrument(skip_all, fields(path = %logical_path.display()))]
pub async fn restore_file(
    recipe: &PackRecipe,
    logical_path: &Path,
    game_dir: &Path,
    fetch: ArtifactFetch<'_>,
) -> PackResult<MaterializedFiles> {
    let wanted = recipe
        .files
        .iter()
        .find(|file| file.logical_path == logical_path)
        .ok_or_else(|| PackError::ManagedIdentityMissing {
            path: logical_path.display().to_string(),
        })?;
    install_selected_files(&recipe.files, &[wanted], game_dir, None, fetch).await
}

/// Reconstructs server-applicable content without reading the source archive.
/// An addressless entry may be copied from the mutable client game directory;
/// every other artifact comes from the verified shared cache or its recorded
/// source.
///
/// The caller must supply a staging game directory. This function writes
/// several ordered layers and intentionally relies on the owning server-create
/// transaction to discard that staging root if any step fails.
#[instrument(skip_all, fields(game_dir = %request.game_dir.display()))]
pub async fn materialize_server_recipe(
    request: ServerMaterializeRequest<'_>,
) -> PackResult<MaterializedServerRecipe> {
    let ServerMaterializeRequest {
        recipe,
        unknown_server_files,
        game_dir,
        client_game_dir,
        client_source_dir,
        server_source_dir,
        fetch,
    } = request;
    let plan = recipe.server_reconstruction_plan(unknown_server_files)?;
    for layer in &plan.layers {
        let Some(relative) = &layer.retained_at else {
            return Err(PackError::RetainedLayerUnavailable {
                scope: layer.scope.as_str(),
                path: client_source_dir.to_path_buf(),
            });
        };
        validate_retained_root(
            &SourceStore::new(client_source_dir),
            relative,
            layer.scope.as_str(),
        )?;
    }

    let files = install_selected_files(
        &recipe.files,
        &plan.files,
        game_dir,
        Some(client_game_dir),
        fetch,
    )
    .await?;
    let client_store = SourceStore::new(client_source_dir);
    let layers = apply_retained_overrides(&client_store, recipe, PackSide::Server, game_dir)?;
    remove_retained_eula(game_dir)?;
    let server_store = SourceStore::new(server_source_dir);
    copy_server_source(&client_store, &server_store, recipe, &plan.layers)?;
    let recipe_path = server_store.write_recipe(&server_desired_state(recipe, &plan))?;

    Ok(MaterializedServerRecipe {
        files,
        layers,
        recipe_path,
        unconfirmed_unknown: plan
            .unconfirmed_unknown
            .iter()
            .map(|file| file.logical_path.clone())
            .collect(),
    })
}

/// The recipe a reconstructed server keeps, describing the server rather than
/// the client it was copied from.
///
/// The two differ wherever a side needed a decision. A file the client took
/// because it is `optional` there may be `unknown` on the server, and an
/// undeclared server side is only installed when this reconstruction confirmed
/// it - so carrying the client's answer across would leave the server's own
/// record claiming a jar that was never written. Recording what this
/// reconstruction actually installed is what lets a later managed-mod operation
/// read the file and know what the server has.
fn server_desired_state(recipe: &PackRecipe, plan: &ServerReconstructionPlan<'_>) -> PackRecipe {
    let installed: BTreeSet<&Path> = plan
        .files
        .iter()
        .map(|file| file.logical_path.as_path())
        .collect();
    let mut retained = recipe.clone();
    for file in &mut retained.files {
        if file.server.needs_decision() {
            file.selected = Some(installed.contains(file.logical_path.as_path()));
        }
    }
    retained
}

/// EULA consent is launcher-global and explicit. A retained pack file is not a
/// consent record, regardless of whether it says `true` or `false`.
fn remove_retained_eula(game_dir: &Path) -> PackResult<()> {
    let path = game_dir.join("eula.txt");
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() => {
            std::fs::remove_dir_all(&path).map_err(|source| PackError::Unwritable { path, source })
        }
        Ok(_) => {
            std::fs::remove_file(&path).map_err(|source| PackError::Unwritable { path, source })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(PackError::Unwritable { path, source }),
    }
}

async fn install_selected_files(
    all: &[PackFileRecipe],
    wanted: &[&PackFileRecipe],
    game_dir: &Path,
    // Where a file the recipe records no address for can be copied from, for
    // the one caller that has such a place: reconstructing a linked server
    // from a client. `None` everywhere else, where a recipe entry with no
    // address is a recipe that cannot be installed.
    client_game_dir: Option<&Path>,
    fetch: ArtifactFetch<'_>,
) -> PackResult<MaterializedFiles> {
    let cancel = fetch.cancel;
    preflight_destinations(wanted, game_dir)?;
    let mut report = MaterializedFiles {
        skipped: all
            .iter()
            .filter(|file| !wanted.iter().any(|wanted| std::ptr::eq(*wanted, *file)))
            .map(|file| file.logical_path.clone())
            .collect(),
        ..MaterializedFiles::default()
    };
    if wanted.is_empty() {
        return Ok(report);
    }

    // A jar the user put in the client themselves is recorded with no address
    // to fetch it from, because there is none: the digest lookup that named it
    // learned what it is, not where it came from. Prefer that installed copy.
    // If it is absent, leave the entry for `fetch_all`: that shared path can
    // still use a verified cache copy without an address, and reports the
    // ordinary missing-source failure only when neither copy exists.
    let mut downloadable: Vec<&PackFileRecipe> = wanted.to_vec();
    if let Some(client_game_dir) = client_game_dir {
        let local: Vec<&PackFileRecipe>;
        (local, downloadable) = wanted.iter().partition(|file| {
            file.downloads.is_empty() && client_game_dir.join(&file.logical_path).is_file()
        });
        for file in local {
            if cancel.is_cancelled() {
                return Err(PackError::Canceled);
            }
            let source = client_game_dir.join(&file.logical_path);
            let destination = game_dir.join(&file.logical_path);
            copy_into_instance(&source, &destination)?;
            report.bytes = report
                .bytes
                .saturating_add(std::fs::metadata(&destination).map_or(0, |data| data.len()));
            report.installed.push(file.logical_path.clone());
        }
    }
    if downloadable.is_empty() {
        return Ok(report);
    }
    let wanted = downloadable.as_slice();

    let outcomes = fetch_all(wanted, fetch).await?;

    for (file, outcome) in wanted.iter().zip(outcomes.iter()) {
        if cancel.is_cancelled() {
            return Err(PackError::Canceled);
        }
        let destination = game_dir.join(&file.logical_path);
        copy_into_instance(&outcome.path, &destination)?;
        if outcome.source == ephervex_download::ArtifactSource::AlreadyCached {
            report.reused_from_cache += 1;
        }
        report.bytes = report.bytes.saturating_add(outcome.bytes);
        report.installed.push(file.logical_path.clone());
    }

    info!(
        installed = report.installed.len(),
        skipped = report.skipped.len(),
        reused = report.reused_from_cache,
        "installed the pack's files"
    );
    Ok(report)
}

fn preflight_destinations(wanted: &[&PackFileRecipe], game_dir: &Path) -> PackResult<()> {
    let mut claimed = BTreeSet::new();
    for file in wanted {
        let raw = file
            .logical_path
            .to_str()
            .ok_or_else(|| PackError::EntryPathUnsafe {
                entry: file.display_name(),
            })?;
        let safe = safe_relative_path(raw.as_bytes(), 0)
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| PackError::EntryPathUnsafe {
                entry: file.display_name(),
            })?;
        if safe != file.logical_path {
            return Err(PackError::EntryPathUnsafe {
                entry: file.display_name(),
            });
        }
        if is_reserved(&safe) {
            return Err(PackError::EntryReserved {
                entry: file.display_name(),
            });
        }
        if !claimed.insert(file.logical_path.as_path()) {
            return Err(PackError::RecipeDestinationCollision {
                path: file.logical_path.clone(),
            });
        }
        let destination =
            contained_join(game_dir, &safe).ok_or_else(|| PackError::EntryPathUnsafe {
                entry: file.display_name(),
            })?;
        if destination.exists() {
            return Err(PackError::RecipeDestinationOccupied { path: destination });
        }
    }
    Ok(())
}

/// Fetches every file into the verified shared cache.
///
/// Shared by pack materialization and managed-mod installation so there is one
/// definition of "fetch these files": probe the shared cache, then one batch of
/// whatever is left for the download manager's bounded concurrency and
/// aggregated progress, falling back to a per-file, per-address walk when the
/// batch fails. A `downloads` array with more than one entry is a mirror list,
/// and using only the first would throw away the source's own answer to a CDN
/// outage.
///
/// # Why the cache is probed here rather than left to the manager
///
/// The manager would find the same entry on its own, and for an artifact that
/// needs no credential nothing would be gained. The difference is the artifact
/// that does: asking a provider for one means reading a secret store, and
/// failing when it holds nothing. A file whose verified copy is already on this
/// machine must not fail for want of a key it is not going to spend, so the
/// probe decides which files are genuinely going to be fetched and only those
/// ask for a credential.
///
/// Returns outcomes positionally matching `files`.
pub(crate) async fn fetch_all(
    files: &[&PackFileRecipe],
    fetch: ArtifactFetch<'_>,
) -> PackResult<Vec<DownloadOutcome>> {
    let mut outcomes: Vec<Option<DownloadOutcome>> = (0..files.len()).map(|_| None).collect();
    let mut missing = Vec::new();
    for (index, file) in files.iter().enumerate() {
        if fetch.cancel.is_cancelled() {
            return Err(PackError::Canceled);
        }
        // A file with no declared address at all - a provider refused to
        // serve it, and nothing has fetched it over the network - can still
        // install if a verified copy is already in the shared cache (an
        // earlier import, or a folder a watcher is minding, put it there).
        // There is no `DownloadRequest` to build without a URL, so this asks
        // the cache directly instead of going through `primary_request`.
        if file.downloads.is_empty() {
            match probe_cache_without_url(file, fetch).await? {
                Some(outcome) => outcomes[index] = Some(outcome),
                None => missing.push(index),
            }
            continue;
        }
        // Built without a credential: the probe only reads a path this crate
        // derived from the file's own strongest digest.
        let probe = primary_request(file, fetch, None)?;
        match fetch.manager.cached(&probe).await {
            Ok(Some(outcome)) => outcomes[index] = Some(outcome),
            // Nothing usable is there. An entry that failed revalidation has
            // already been discarded by the probe, so this is the ordinary
            // "fetch it" answer.
            Ok(None) => missing.push(index),
            // The cache could not be read, which is not the same thing and must
            // not be recovered from by fetching. A local storage fault says
            // nothing about whether the network is needed, and treating it as a
            // miss would go on to ask a provider for a credential - turning an
            // unreadable cache path into "your API key is missing", for an
            // artifact no request was ever going to be made for.
            Err(source) => {
                return Err(PackError::DownloadFailed {
                    path: file.display_name(),
                    source: Box::new(source),
                });
            }
        }
    }
    if missing.is_empty() {
        return Ok(outcomes.into_iter().flatten().collect());
    }
    let wanted: Vec<&PackFileRecipe> = missing.iter().map(|index| files[*index]).collect();
    let fetched = fetch_missing(&wanted, fetch).await?;
    for (index, outcome) in missing.into_iter().zip(fetched) {
        outcomes[index] = Some(outcome);
    }
    Ok(outcomes.into_iter().flatten().collect())
}

/// Fetches the files the shared cache does not already hold.
async fn fetch_missing(
    files: &[&PackFileRecipe],
    fetch: ArtifactFetch<'_>,
) -> PackResult<Vec<DownloadOutcome>> {
    let requests: Vec<DownloadRequest> = files
        .iter()
        .map(|file| {
            let credential = credential_for(file, fetch)?;
            primary_request(file, fetch, credential.as_ref())
        })
        .collect::<PackResult<_>>()?;

    match fetch
        .manager
        .download_all(&requests, fetch.reporter, fetch.cancel)
        .await
    {
        // The batch answers positionally, and the caller places each outcome
        // against the file that asked for it. A short answer is therefore
        // treated as a failed batch rather than trusted, because the
        // alternative is installing fewer files than the recipe named without
        // saying so.
        Ok(outcomes) if outcomes.len() == requests.len() => Ok(outcomes),
        Err(failure) if failure.is_cancellation() => Err(PackError::Canceled),
        outcome => {
            if let Err(failure) = &outcome {
                let path = files
                    .get(failure.index())
                    .map(|file| file.display_name())
                    .unwrap_or_default();
                warn!(
                    error = %failure,
                    %path,
                    "a download failed; retrying each file against its remaining addresses"
                );
            }
            fetch_one_by_one(files, fetch).await
        }
    }
}

/// The credential this file's provider requires to serve it, if any.
///
/// Decided from the durable provider identity the recipe records and from
/// nothing else - not the address, not the filename, not the format the pack was
/// imported from - so a mixed desired state offers each provider's secret only
/// to its own artifacts.
fn credential_for(
    file: &PackFileRecipe,
    fetch: ArtifactFetch<'_>,
) -> PackResult<Option<Arc<AuthenticatedSource>>> {
    fetch
        .credentials
        .credential_for(file.identity.as_ref())
        .map_err(|source| PackError::ArtifactCredentialUnavailable {
            path: file.display_name(),
            source,
        })
}

/// One download request per file, using its first permitted address.
fn primary_request(
    file: &PackFileRecipe,
    fetch: ArtifactFetch<'_>,
    credential: Option<&Arc<AuthenticatedSource>>,
) -> PackResult<DownloadRequest> {
    let address = file
        .downloads
        .first()
        .ok_or_else(|| PackError::FileDownloadsMissing {
            path: file.display_name(),
        })?;
    request_for(file, address, fetch, credential)
}

/// Builds a verified request for one address.
fn request_for(
    file: &PackFileRecipe,
    address: &str,
    fetch: ArtifactFetch<'_>,
    credential: Option<&Arc<AuthenticatedSource>>,
) -> PackResult<DownloadRequest> {
    let hash = strongest_hash(file)?;
    let destination =
        fetch
            .cache
            .content(ContentStore::Downloads, hash, extension_of(file).as_ref());

    // The provider parser applied its domain allowlist when the address
    // arrived, and the file's scope applies it again when it is sent. Parse the
    // serialized recipe value without repeating the downloader's transport
    // policy so loopback integration tests can exercise the full pipeline.
    let url = Url::parse(address).map_err(|_| PackError::DownloadUrlRejected {
        path: file.display_name(),
        source: UrlPolicyError::Malformed {
            url: address.to_owned(),
        },
    })?;
    let mut request = DownloadRequest::new(url, destination)
        // Every published hash is checked, not just the one that chose the
        // cache path.
        .verifying_all(file.hashes.clone())
        // Display only. A label from pack metadata never chooses a path.
        .labeled(file.display_name());
    if let Some(size) = file.size {
        request = request.sized(size);
    }
    if let Some(source) = credential {
        // The source decides whether this particular address may receive the
        // credential; attaching it here only offers it.
        request = request.authenticated_by(Arc::clone(source));
    }
    let scope = fetch
        .credentials
        .hosts_for(file.identity.as_ref())
        .map_err(|source| PackError::DownloadFailed {
            path: file.display_name(),
            source: Box::new(source),
        })?;
    if let Some(scope) = scope {
        request = request.within(scope);
    }
    Ok(request)
}

/// Checks the shared cache directly for a file that names no download
/// address at all, with no network access.
///
/// The counterpart to [`DownloadManager::cached`] for a file
/// [`request_for`]/[`primary_request`] cannot even build a request for: both
/// ask the identical question ("is a verified copy of this hash already at
/// its content-addressed path?"), but this one has no `DownloadRequest` to
/// ask it through, since building one needs a URL this file does not have.
/// `Ok(None)` covers both an ordinary cache miss and a file with no usable
/// hash to check against; only a genuine filesystem fault propagates.
async fn probe_cache_without_url(
    file: &PackFileRecipe,
    fetch: ArtifactFetch<'_>,
) -> PackResult<Option<DownloadOutcome>> {
    let Ok(hash) = strongest_hash(file) else {
        return Ok(None);
    };
    let destination =
        fetch
            .cache
            .content(ContentStore::Downloads, hash, extension_of(file).as_ref());
    let io_failed = |source: std::io::Error| PackError::DownloadFailed {
        path: file.display_name(),
        source: Box::new(DownloadError::io(
            ArtifactStep::ReadCached,
            &destination,
            source,
        )),
    };
    let metadata = match std::fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_failed(source)),
    };
    if !file_matches_hashes(&destination, &file.hashes)
        .await
        .map_err(|error| PackError::DownloadFailed {
            path: file.display_name(),
            source: Box::new(error),
        })?
    {
        return Ok(None);
    }
    Ok(Some(DownloadOutcome {
        path: destination,
        bytes: metadata.len(),
        source: ArtifactSource::AlreadyCached,
    }))
}

/// The hash a file's cache path is derived from.
///
/// The list is ordered strongest-first when the recipe is built, so this is the
/// first entry; it is a function rather than an index so the ordering rule has a
/// name and a failure has an error.
fn strongest_hash(file: &PackFileRecipe) -> PackResult<&Hash> {
    file.hashes
        .first()
        .ok_or_else(|| PackError::FileHashesMissing {
            path: file.display_name(),
        })
}

/// The cache filename's extension, taken from the *logical* path.
///
/// `None` when the path has no extension or one that is not a plain suffix. The
/// extension is cosmetic - it exists so a human browsing the cache can tell a
/// `.jar` from a `.zip` - so an unusable one is dropped rather than fatal.
fn extension_of(file: &PackFileRecipe) -> Option<FileExtension> {
    file.logical_path
        .extension()
        .and_then(|extension| extension.to_str())
        .and_then(|extension| FileExtension::new(extension).ok())
}

/// Fetches each file sequentially, trying every address it lists.
async fn fetch_one_by_one(
    files: &[&PackFileRecipe],
    fetch: ArtifactFetch<'_>,
) -> PackResult<Vec<DownloadOutcome>> {
    let cancel = fetch.cancel;
    let mut outcomes = Vec::with_capacity(files.len());
    for file in files {
        if cancel.is_cancelled() {
            return Err(PackError::Canceled);
        }
        if file.downloads.is_empty() {
            return Err(PackError::FileDownloadsMissing {
                path: file.display_name(),
            });
        }
        let credential = credential_for(file, fetch)?;
        let mut last_failure = None;
        let mut fetched = None;
        for address in &file.downloads {
            let request = request_for(file, address, fetch, credential.as_ref())?;
            // Whether the credential was *sent*, not merely offered: a source
            // declines to attach itself to an address outside its permitted
            // hosts, and a refusal from such an address is nobody's credential
            // problem.
            let authenticated = credential
                .as_ref()
                .is_some_and(|source| source.permits(request.url()));
            match fetch
                .manager
                .download(&request, fetch.reporter, cancel)
                .await
            {
                Ok(outcome) => {
                    fetched = Some(outcome);
                    break;
                }
                Err(error) if error.is_cancellation() => return Err(PackError::Canceled),
                Err(error) => {
                    warn!(file = %file.display_name(), %error, "a pack download address failed");
                    last_failure = Some((error, authenticated));
                }
            }
        }
        match (fetched, last_failure) {
            (Some(outcome), _) => outcomes.push(outcome),
            // The loop ran at least once - an empty address list was refused
            // above - so a file that was not fetched has a failure to report.
            (None, Some((error, authenticated))) => {
                return Err(download_failure(file, fetch, authenticated, error));
            }
            (None, None) => {
                return Err(PackError::FileDownloadsMissing {
                    path: file.display_name(),
                });
            }
        }
    }
    Ok(outcomes)
}

/// Turns a failed transfer into the failure a caller should act on.
///
/// Almost always a download failure, and deliberately so: a refusal from an
/// address is normally about the address. The exception is a request that
/// carried a provider's credential and came back with a status that provider
/// uses to refuse one - there the actionable fact is the credential, not the
/// download, and only the layer that issued it can say so. See
/// [`ArtifactCredentials::rejected`].
fn download_failure(
    file: &PackFileRecipe,
    fetch: ArtifactFetch<'_>,
    authenticated: bool,
    error: DownloadError,
) -> PackError {
    if authenticated
        && let Some(status) = refused_with(&error)
        && let Some(source) = fetch.credentials.rejected(file.identity.as_ref(), status)
    {
        return PackError::ArtifactCredentialUnavailable {
            path: file.display_name(),
            source,
        };
    }
    PackError::DownloadFailed {
        path: file.display_name(),
        source: Box::new(error),
    }
}

/// The HTTP status a transfer ended on, seen through the retry wrapper.
///
/// `None` for everything that is not a status at all - a transport fault, a
/// cancellation, a local write - because none of those is a provider saying
/// anything about a credential.
fn refused_with(error: &DownloadError) -> Option<u16> {
    match error {
        DownloadError::HttpStatus { status, .. } => Some(*status),
        DownloadError::RetriesExhausted { source, .. } => refused_with(source),
        _ => None,
    }
}

/// Copies a verified cache entry into the instance.
pub(crate) fn copy_into_instance(source: &Path, destination: &Path) -> PackResult<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|source| PackError::Unwritable {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::copy(source, destination).map_err(|error| PackError::Unwritable {
        path: destination.to_path_buf(),
        source: error,
    })?;
    Ok(())
}

/// One override layer that was applied to the game directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedLayer {
    pub layer: OverrideLayer,
    pub files: u64,
    /// The `mods/*.jar` entries this layer placed, relative to the game
    /// directory. The pack shipped these bytes rather than a reference to
    /// them, so the recipe learns them here or never.
    pub mod_files: Vec<PathBuf>,
}

/// Extracts the override layers one side takes, in the required order.
///
/// Common first, then the side-specific layer, so the later layer wins.
#[instrument(skip_all, fields(side = side.as_str()))]
pub fn apply_overrides(
    archive: &mut PackArchive,
    recipe: &PackRecipe,
    side: PackSide,
    game_dir: &Path,
) -> PackResult<Vec<AppliedLayer>> {
    let layers: Vec<OverrideLayer> = recipe.layers_for(side).into_iter().cloned().collect();
    let mut applied = Vec::with_capacity(layers.len());
    for layer in layers {
        // Recorded as the reader walks the layer, which is the only moment the
        // per-file contents of one are known: the index names the layer by its
        // prefix and never lists what is inside it. An entry recorded here and
        // then refused takes the whole extraction down with it, and the recipe
        // is only written once extraction has succeeded.
        let mut mod_files = Vec::new();
        let report =
            archive.extract_prefix_filtered(&layer.archive_prefix, game_dir, &mut |path| {
                if is_mod_file_path(path) {
                    mod_files.push(path.to_path_buf());
                }
                EntryDecision::Extract
            })?;
        debug!(
            layer = layer.scope.as_str(),
            files = report.files,
            mods = mod_files.len(),
            "applied a pack override layer"
        );
        applied.push(AppliedLayer {
            layer,
            files: report.files,
            mod_files,
        });
    }
    Ok(applied)
}

/// Applies retained common and side-specific layers in established precedence
/// order, rejecting missing layers and every link or special filesystem entry.
pub fn apply_retained_overrides(
    store: &SourceStore,
    recipe: &PackRecipe,
    side: PackSide,
    game_dir: &Path,
) -> PackResult<Vec<AppliedLayer>> {
    let mut applied = Vec::new();
    let limits = ExtractionLimits::default();
    let mut spent = ExtractionReport::default();
    for layer in recipe.layers_for(side) {
        let relative =
            layer
                .retained_at
                .as_ref()
                .ok_or_else(|| PackError::RetainedLayerUnavailable {
                    scope: layer.scope.as_str(),
                    path: store.dir().to_path_buf(),
                })?;
        let source = validate_retained_root(store, relative, layer.scope.as_str())?;
        let report = copy_retained_tree(&source, game_dir, &limits, &mut spent)?;
        applied.push(AppliedLayer {
            layer: layer.clone(),
            files: report.files,
            // Nothing to learn here. The recipe this is rebuilding from already
            // records what its layers place, from the import that retained them.
            mod_files: Vec::new(),
        });
    }
    Ok(applied)
}

fn validate_retained_root(
    store: &SourceStore,
    relative: &Path,
    scope: &'static str,
) -> PackResult<PathBuf> {
    let raw = relative
        .to_str()
        .ok_or_else(|| PackError::RetainedLayerUnavailable {
            scope,
            path: store.resolve(relative),
        })?;
    let safe = safe_relative_path(raw.as_bytes(), 0).filter(|path| !path.as_os_str().is_empty());
    let Some(safe) = safe else {
        return Err(PackError::RetainedLayerUnavailable {
            scope,
            path: store.resolve(relative),
        });
    };
    let source =
        contained_join(store.dir(), &safe).ok_or_else(|| PackError::RetainedLayerUnavailable {
            scope,
            path: store.resolve(relative),
        })?;
    let metadata =
        std::fs::symlink_metadata(&source).map_err(|_| PackError::RetainedLayerUnavailable {
            scope,
            path: source.clone(),
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PackError::RetainedLayerUnavailable {
            scope,
            path: source,
        });
    }
    let canonical_store =
        std::fs::canonicalize(store.dir()).map_err(|_| PackError::RetainedLayerUnavailable {
            scope,
            path: store.dir().to_path_buf(),
        })?;
    let canonical_source =
        std::fs::canonicalize(&source).map_err(|_| PackError::RetainedLayerUnavailable {
            scope,
            path: source.clone(),
        })?;
    if !canonical_source.starts_with(canonical_store) {
        return Err(PackError::RetainedLayerUnavailable {
            scope,
            path: source,
        });
    }
    Ok(source)
}

fn copy_retained_tree(
    source: &Path,
    destination: &Path,
    limits: &ExtractionLimits,
    spent: &mut ExtractionReport,
) -> PackResult<ExtractionReport> {
    let mut report = ExtractionReport::default();
    copy_retained_directory(source, source, destination, limits, spent, &mut report)?;
    Ok(report)
}

fn copy_retained_directory(
    root: &Path,
    source: &Path,
    destination: &Path,
    limits: &ExtractionLimits,
    spent: &mut ExtractionReport,
    report: &mut ExtractionReport,
) -> PackResult<()> {
    std::fs::create_dir_all(destination).map_err(|source| PackError::Unwritable {
        path: destination.to_path_buf(),
        source,
    })?;
    for entry in std::fs::read_dir(source).map_err(|error| PackError::Unwritable {
        path: source.to_path_buf(),
        source: error,
    })? {
        let entry = entry.map_err(|error| PackError::Unwritable {
            path: source.to_path_buf(),
            source: error,
        })?;
        let from = entry.path();
        copy_retained_entry(root, &from, destination, limits, spent, report)?;
    }
    Ok(())
}

fn copy_retained_entry(
    root: &Path,
    from: &Path,
    destination: &Path,
    limits: &ExtractionLimits,
    spent: &mut ExtractionReport,
    report: &mut ExtractionReport,
) -> PackResult<()> {
    let relative = from
        .strip_prefix(root)
        .map_err(|_| PackError::EntryPathUnsafe {
            entry: from.display().to_string(),
        })?;
    let raw = relative
        .to_str()
        .ok_or_else(|| PackError::EntryPathUnsafe {
            entry: relative.display().to_string(),
        })?;
    if safe_relative_path(raw.as_bytes(), 0).as_deref() != Some(relative) {
        return Err(PackError::EntryPathUnsafe {
            entry: raw.to_owned(),
        });
    }
    if is_reserved(relative) {
        return Err(PackError::EntryReserved {
            entry: raw.to_owned(),
        });
    }
    let kind = std::fs::symlink_metadata(from)
        .map_err(|source| PackError::Unwritable {
            path: from.to_path_buf(),
            source,
        })?
        .file_type();
    let to = destination.join(relative);
    if kind.is_symlink() {
        return Err(PackError::EntryTypeUnsupported {
            entry: raw.to_owned(),
            kind: "symlink",
        });
    }
    if kind.is_dir() {
        let entries = spent
            .files
            .saturating_add(spent.directories)
            .saturating_add(1);
        if entries > limits.max_entries {
            return Err(PackError::LimitExceeded {
                limit: "entry-count",
                allowed: limits.max_entries,
                reached: entries,
            });
        }
        spent.directories = spent.directories.saturating_add(1);
        report.directories = report.directories.saturating_add(1);
        std::fs::create_dir_all(&to)
            .map_err(|source| PackError::Unwritable { path: to, source })?;
        copy_retained_directory(root, from, destination, limits, spent, report)?;
        return Ok(());
    }
    if !kind.is_file() {
        return Err(PackError::EntryTypeUnsupported {
            entry: raw.to_owned(),
            kind: "special file",
        });
    }
    let size = std::fs::metadata(from)
        .map_err(|source| PackError::Unwritable {
            path: from.to_path_buf(),
            source,
        })?
        .len();
    charge_retained_file(size, limits, spent)?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|source| PackError::Unwritable {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::copy(from, &to).map_err(|source| PackError::Unwritable { path: to, source })?;
    report.files = report.files.saturating_add(1);
    report.bytes = report.bytes.saturating_add(size);
    Ok(())
}

fn charge_retained_file(
    size: u64,
    limits: &ExtractionLimits,
    spent: &mut ExtractionReport,
) -> PackResult<()> {
    let entries = spent
        .files
        .saturating_add(spent.directories)
        .saturating_add(1);
    let bytes = spent.bytes.saturating_add(size);
    for (limit, allowed, reached) in [
        ("entry-count", limits.max_entries, entries),
        ("entry-size", limits.max_entry_bytes, size),
        ("total-size", limits.max_total_bytes, bytes),
    ] {
        if reached > allowed {
            return Err(PackError::LimitExceeded {
                limit,
                allowed,
                reached,
            });
        }
    }
    spent.files = spent.files.saturating_add(1);
    spent.bytes = bytes;
    Ok(())
}

fn copy_server_source(
    client: &SourceStore,
    server: &SourceStore,
    recipe: &PackRecipe,
    layers: &[&OverrideLayer],
) -> PackResult<()> {
    let limits = ExtractionLimits::default();
    let mut spent = ExtractionReport::default();
    for layer in layers {
        let relative =
            layer
                .retained_at
                .as_ref()
                .ok_or_else(|| PackError::RetainedLayerUnavailable {
                    scope: layer.scope.as_str(),
                    path: client.dir().to_path_buf(),
                })?;
        let source = validate_retained_root(client, relative, layer.scope.as_str())?;
        copy_retained_tree(&source, &server.resolve(relative), &limits, &mut spent)?;
    }
    if let Some(relative) = &recipe.provider_metadata.manifest_file {
        let raw = relative
            .to_str()
            .ok_or_else(|| PackError::EntryPathUnsafe {
                entry: relative.display().to_string(),
            })?;
        let safe = safe_relative_path(raw.as_bytes(), 0)
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| PackError::EntryPathUnsafe {
                entry: relative.display().to_string(),
            })?;
        if safe != *relative {
            return Err(PackError::EntryPathUnsafe {
                entry: relative.display().to_string(),
            });
        }
        let source =
            contained_join(client.dir(), &safe).ok_or_else(|| PackError::EntryPathUnsafe {
                entry: relative.display().to_string(),
            })?;
        let kind = std::fs::symlink_metadata(&source).map_err(|source_error| {
            PackError::RecipeUnreadable {
                path: source.clone(),
                source: source_error,
            }
        })?;
        if kind.file_type().is_symlink() || !kind.is_file() {
            return Err(PackError::EntryTypeUnsupported {
                entry: relative.display().to_string(),
                kind: "non-regular retained manifest",
            });
        }
        let canonical_client = std::fs::canonicalize(client.dir()).map_err(|source_error| {
            PackError::RecipeUnreadable {
                path: client.dir().to_path_buf(),
                source: source_error,
            }
        })?;
        let canonical_source =
            std::fs::canonicalize(&source).map_err(|source_error| PackError::RecipeUnreadable {
                path: source.clone(),
                source: source_error,
            })?;
        if !canonical_source.starts_with(canonical_client) {
            return Err(PackError::EntryPathUnsafe {
                entry: relative.display().to_string(),
            });
        }
        let destination =
            contained_join(server.dir(), &safe).ok_or_else(|| PackError::EntryPathUnsafe {
                entry: relative.display().to_string(),
            })?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|source| PackError::Unwritable {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::copy(&source, &destination).map_err(|source| PackError::Unwritable {
            path: destination,
            source,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::{HashAlgorithm, ImportSourceKind, LoaderSelection};

    use super::*;
    use ephervex_model::ModOrigin;

    use crate::credentials::OpenArtifacts;
    use crate::recipe::{OverrideScope, ProviderRecipeMetadata, RECIPE_VERSION, SideRequirement};

    const SHA1: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
    const SHA512: &str = "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e";

    fn file(path: &str) -> PackFileRecipe {
        PackFileRecipe {
            logical_path: PathBuf::from(path),
            hashes: vec![
                Hash::new(HashAlgorithm::Sha512, SHA512).expect("sha512"),
                Hash::new(HashAlgorithm::Sha1, SHA1).expect("sha1"),
            ],
            downloads: vec!["https://cdn.modrinth.com/data/x/a.jar".to_owned()],
            size: Some(10),
            client: SideRequirement::Required,
            server: SideRequirement::Required,
            selected: None,
            origin: ModOrigin::PackDeclared,
            identity: None,
            relations: None,
            original_identity: None,
        }
    }

    /// Copies a retained override layer the way `apply_retained_overrides`
    /// does, without needing a whole recipe to reach it.
    fn copy_layer(source: &Path, destination: &Path) -> PackResult<ExtractionReport> {
        let limits = ExtractionLimits::default();
        let mut spent = ExtractionReport::default();
        copy_retained_tree(source, destination, &limits, &mut spent)
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_in_a_retained_layer_is_refused_rather_than_followed() {
        // Retained layers come from an untrusted archive that was extracted
        // earlier, so the same refusal has to apply on the way back out. A
        // link planted here would otherwise write through it into the user's
        // home directory the next time a server was reconstructed.
        let dir = tempfile::tempdir().expect("temp dir");
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"do not touch").expect("write");

        let layer = dir.path().join("layer");
        std::fs::create_dir_all(layer.join("config")).expect("create");
        std::fs::write(layer.join("config/ok.toml"), b"a=1").expect("write");
        std::os::unix::fs::symlink(&outside, layer.join("config/escape.toml"))
            .expect("plant a symlink");

        let destination = dir.path().join("minecraft");
        let error = copy_layer(&layer, &destination).expect_err("a symlink is refused");
        assert!(
            matches!(
                error,
                PackError::EntryTypeUnsupported {
                    kind: "symlink",
                    ..
                }
            ),
            "unexpected error: {error:?}"
        );
        assert_eq!(
            std::fs::read(&outside).expect("read"),
            b"do not touch",
            "nothing was written through the link"
        );
        assert!(!destination.join("config/escape.toml").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_in_a_retained_layer_is_refused_too() {
        // The directory case is the one that would otherwise let a whole tree
        // be walked from outside the layer.
        let dir = tempfile::tempdir().expect("temp dir");
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("create");
        std::fs::write(elsewhere.join("secret.txt"), b"private").expect("write");

        let layer = dir.path().join("layer");
        std::fs::create_dir_all(&layer).expect("create");
        std::os::unix::fs::symlink(&elsewhere, layer.join("config")).expect("plant a symlink");

        let error = copy_layer(&layer, &dir.path().join("minecraft"))
            .expect_err("a symlinked directory is refused");
        assert!(
            matches!(
                error,
                PackError::EntryTypeUnsupported {
                    kind: "symlink",
                    ..
                }
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn a_retained_layer_entry_over_its_ceiling_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let layer = dir.path().join("layer");
        std::fs::create_dir_all(&layer).expect("create");
        std::fs::write(layer.join("huge.bin"), vec![b'x'; 4096]).expect("write");

        let limits = ExtractionLimits {
            max_entry_bytes: 1024,
            ..ExtractionLimits::default()
        };
        let mut spent = ExtractionReport::default();
        let error = copy_retained_tree(&layer, &dir.path().join("out"), &limits, &mut spent)
            .expect_err("an oversize retained file is refused");
        assert!(
            matches!(
                error,
                PackError::LimitExceeded {
                    limit: "entry-size",
                    ..
                }
            ),
            "unexpected error: {error:?}"
        );
    }

    /// A fetch context that never touches the network, for building requests.
    fn offline_fetch<'a>(
        manager: &'a DownloadManager,
        cache: &'a CacheLayout,
        reporter: &'a OperationReporter,
        cancel: &'a CancellationToken,
    ) -> ArtifactFetch<'a> {
        ArtifactFetch {
            manager,
            cache,
            credentials: &OpenArtifacts,
            reporter,
            cancel,
        }
    }

    #[test]
    fn a_cache_path_is_derived_from_the_strongest_hash_and_never_from_the_url() {
        // a remote server does not get to choose where a file
        // lands. The destination comes from the hash the pack published.
        let cache = CacheLayout::new("/cache/ephervex");
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let reporter = OperationReporter::silent("test");
        let cancel = CancellationToken::new();
        let fetch = offline_fetch(&manager, &cache, &reporter, &cancel);
        let request = primary_request(&file("mods/some mod.jar"), fetch, None).expect("request");
        let destination = request.destination().to_string_lossy().into_owned();

        assert!(destination.starts_with("/cache/ephervex/downloads/sha512/cf/"));
        assert!(destination.ends_with(&format!("{SHA512}.jar")));
        assert!(!destination.contains("some mod"));
        assert_eq!(
            request.expected_hashes().len(),
            2,
            "the verifier checks both"
        );
        assert_eq!(request.expected_size(), Some(10));
        assert!(
            request.authenticated_source().is_none(),
            "an open source's download carries no credential"
        );
    }

    #[test]
    fn a_path_with_no_usable_extension_still_gets_a_cache_path() {
        let cache = CacheLayout::new("/cache/ephervex");
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let reporter = OperationReporter::silent("test");
        let cancel = CancellationToken::new();
        let mut entry = file("config/no-extension");
        entry.logical_path = PathBuf::from("config/no-extension");
        let request = primary_request(
            &entry,
            offline_fetch(&manager, &cache, &reporter, &cancel),
            None,
        )
        .expect("request");
        assert!(
            request.destination().to_string_lossy().ends_with(SHA512),
            "an extension is cosmetic, not required"
        );
    }

    #[test]
    fn an_operations_credential_reaches_the_request_it_belongs_to_and_no_other() {
        // The cache path is still the content hash, and the credential rides
        // along with the request rather than with the manager.
        let cache = CacheLayout::new("/cache/ephervex");
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let reporter = OperationReporter::silent("test");
        let cancel = CancellationToken::new();
        let source = Arc::new(
            AuthenticatedSource::new(
                reqwest::header::HeaderName::from_static("x-api-key"),
                ephervex_model::SecretString::new("test-key-do-not-use"),
                ephervex_download::DomainAllowlist::curseforge_artifact_defaults(),
                &ephervex_download::HttpConfig::default(),
            )
            .expect("a credential builds its containment client"),
        );

        let authenticated = primary_request(
            &file("mods/a.jar"),
            offline_fetch(&manager, &cache, &reporter, &cancel),
            Some(&source),
        )
        .expect("request");
        assert_eq!(
            authenticated
                .authenticated_source()
                .map(|source| source.header().as_str()),
            Some("x-api-key")
        );
        assert!(
            authenticated
                .destination()
                .to_string_lossy()
                .contains("downloads/sha512/"),
            "the cache address is still the verified content hash"
        );

        let open = primary_request(
            &file("mods/a.jar"),
            offline_fetch(&manager, &cache, &reporter, &cancel),
            None,
        )
        .expect("request");
        assert!(open.authenticated_source().is_none());
    }

    /// [`file`] with no download address, as a watcher-resolved or
    /// otherwise cache-only recipe entry has.
    fn downloadless_file(path: &str) -> PackFileRecipe {
        let mut entry = file(path);
        entry.downloads = Vec::new();
        entry
    }

    #[tokio::test]
    async fn a_downloadless_file_already_verified_in_the_cache_installs_with_no_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = CacheLayout::new(dir.path());
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let reporter = OperationReporter::silent("test");
        let cancel = CancellationToken::new();
        let entry = downloadless_file("mods/a.jar");

        // `file()`'s hashes are the empty body's, so an empty fixture at the
        // strongest hash's cache path is what "already verified" looks like.
        let destination = cache.content(
            ContentStore::Downloads,
            &entry.hashes[0],
            Some(&FileExtension::new("jar").expect("jar")),
        );
        std::fs::create_dir_all(destination.parent().expect("shard")).expect("shard");
        std::fs::write(&destination, []).expect("write fixture");

        let outcomes = fetch_all(
            &[&entry],
            offline_fetch(&manager, &cache, &reporter, &cancel),
        )
        .await
        .expect("a verified cache entry installs with no download address");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].source, ArtifactSource::AlreadyCached);
        assert_eq!(outcomes[0].path, destination);
    }

    #[tokio::test]
    async fn a_downloadless_file_the_cache_does_not_have_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = CacheLayout::new(dir.path());
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let reporter = OperationReporter::silent("test");
        let cancel = CancellationToken::new();
        let entry = downloadless_file("mods/a.jar");

        let error = fetch_all(
            &[&entry],
            offline_fetch(&manager, &cache, &reporter, &cancel),
        )
        .await
        .expect_err("no address and no cache entry cannot install");
        assert!(
            matches!(error, PackError::FileDownloadsMissing { .. }),
            "unexpected error: {error:?}"
        );
    }

    #[tokio::test]
    async fn a_linked_server_uses_a_cached_downloadless_file_when_the_client_copy_is_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = CacheLayout::new(dir.path().join("cache"));
        let manager =
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager");
        let reporter = OperationReporter::silent("test");
        let cancel = CancellationToken::new();
        let entry = downloadless_file("mods/a.jar");
        let client_game = dir.path().join("client");
        let server_game = dir.path().join("server");

        let cached = cache.content(
            ContentStore::Downloads,
            &entry.hashes[0],
            Some(&FileExtension::new("jar").expect("jar")),
        );
        std::fs::create_dir_all(cached.parent().expect("shard")).expect("shard");
        std::fs::write(&cached, []).expect("cache fixture");

        let installed = install_selected_files(
            std::slice::from_ref(&entry),
            &[&entry],
            &server_game,
            Some(&client_game),
            offline_fetch(&manager, &cache, &reporter, &cancel),
        )
        .await
        .expect("the shared cache supplies the linked server");

        assert_eq!(installed.reused_from_cache, 1);
        assert!(server_game.join("mods/a.jar").is_file());
        assert!(
            !client_game.join("mods/a.jar").exists(),
            "the test must not pass by copying the client's jar"
        );
    }

    #[test]
    fn layers_are_applied_common_first_then_side_specific() {
        let recipe = PackRecipe {
            recipe_version: RECIPE_VERSION,
            source_kind: ImportSourceKind::ModrinthPack,
            original_name: "Test".to_owned(),
            original_version: None,
            author: None,
            summary: None,
            archive_name: None,
            minecraft_version: "1.20.1".to_owned(),
            loader: LoaderSelection::vanilla(),
            files: Vec::new(),
            override_layers: vec![
                OverrideLayer {
                    scope: OverrideScope::Client,
                    archive_prefix: "client-overrides".to_owned(),
                    retained_at: None,
                    files: 0,
                },
                OverrideLayer {
                    scope: OverrideScope::Common,
                    archive_prefix: "overrides".to_owned(),
                    retained_at: None,
                    files: 0,
                },
            ],
            provider_metadata: ProviderRecipeMetadata::new(ImportSourceKind::ModrinthPack, None),
        };

        // Declared out of order in the recipe; applied in the required order.
        let ordered: Vec<&str> = recipe
            .layers_for(PackSide::Client)
            .iter()
            .map(|layer| layer.archive_prefix.as_str())
            .collect();
        assert_eq!(ordered, vec!["overrides", "client-overrides"]);
    }
}
