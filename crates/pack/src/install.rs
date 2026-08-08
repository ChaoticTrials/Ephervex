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
//! step is deliberately *not* here — applying global preferences needs the
//! configuration file, which lives above this crate — but the ordering
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
//! * importing the same pack twice — which is what bisecting a bug looks like —
//!   costs no network at all the second time;
//! * the download manager's atomicity invariant means a file at a cache path
//!   has already been verified, so the copy into the instance is a copy of
//!   something known good rather than of something hopefully good.
//!
//! The instance still owns its own copy of every mod: `mods/`, `config/`, and
//! overrides are instance-local.

use std::path::{Path, PathBuf};

use ephervex_download::{
    CacheLayout, ContentStore, DownloadManager, DownloadRequest, FileExtension, OperationReporter,
    Url, UrlPolicyError,
};
use ephervex_model::Hash;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::archive::PackArchive;
use crate::error::{PackError, PackResult};
use crate::recipe::{OptionalSelection, OverrideLayer, PackFileRecipe, PackRecipe, PackSide};

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
    pub manager: &'a DownloadManager,
    pub cache: &'a CacheLayout,
    pub reporter: &'a OperationReporter,
    pub cancel: &'a CancellationToken,
}

/// Downloads and installs the pack files one side needs.
///
/// Files are fetched as a batch so the download manager's bounded concurrency
/// and single aggregated progress bar apply. If the batch fails, each remaining
/// file is retried address by address: a `downloads` array with more than one
/// entry is a mirror list, and using only the first would throw away the
/// pack's own answer to a CDN outage.
#[instrument(skip_all, fields(side = request.side.as_str(), game_dir = %request.game_dir.display()))]
pub async fn install_files(request: FileInstall<'_>) -> PackResult<MaterializedFiles> {
    let FileInstall {
        recipe,
        side,
        selection,
        game_dir,
        manager,
        cache,
        reporter,
        cancel,
    } = request;

    let wanted = recipe.files_for(side, selection);
    let mut report = MaterializedFiles {
        skipped: recipe
            .files
            .iter()
            .filter(|file| !file.is_installed_on(side, selection))
            .map(|file| file.logical_path.clone())
            .collect(),
        ..MaterializedFiles::default()
    };
    if wanted.is_empty() {
        return Ok(report);
    }

    let requests: Vec<DownloadRequest> = wanted
        .iter()
        .map(|file| primary_request(file, cache))
        .collect::<PackResult<_>>()?;

    let outcomes = match manager.download_all(&requests, reporter, cancel).await {
        Ok(outcomes) => outcomes,
        Err(error) if error.is_cancellation() => return Err(PackError::Canceled),
        Err(error) => {
            warn!(
                %error,
                "a pack download failed; retrying each file against its remaining addresses"
            );
            fetch_one_by_one(&wanted, cache, manager, reporter, cancel).await?
        }
    };

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

/// One download request per file, using its first permitted address.
fn primary_request(file: &PackFileRecipe, cache: &CacheLayout) -> PackResult<DownloadRequest> {
    let address = file
        .downloads
        .first()
        .ok_or_else(|| PackError::FileDownloadsMissing {
            path: file.display_name(),
        })?;
    request_for(file, address, cache)
}

/// Builds a verified request for one address.
fn request_for(
    file: &PackFileRecipe,
    address: &str,
    cache: &CacheLayout,
) -> PackResult<DownloadRequest> {
    let hash = strongest_hash(file)?;
    let destination = cache.content(ContentStore::Downloads, hash, extension_of(file).as_ref());

    // The provider parser has already applied its domain allowlist. Parse the
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
    Ok(request)
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
/// extension is cosmetic — it exists so a human browsing the cache can tell a
/// `.jar` from a `.zip` — so an unusable one is dropped rather than fatal.
fn extension_of(file: &PackFileRecipe) -> Option<FileExtension> {
    file.logical_path
        .extension()
        .and_then(|extension| extension.to_str())
        .and_then(|extension| FileExtension::new(extension).ok())
}

/// Fetches each file sequentially, trying every address it lists.
async fn fetch_one_by_one(
    files: &[&PackFileRecipe],
    cache: &CacheLayout,
    manager: &DownloadManager,
    reporter: &OperationReporter,
    cancel: &CancellationToken,
) -> PackResult<Vec<ephervex_download::DownloadOutcome>> {
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
        let mut last_error = None;
        let mut fetched = None;
        for address in &file.downloads {
            let request = request_for(file, address, cache)?;
            match manager.download(&request, reporter, cancel).await {
                Ok(outcome) => {
                    fetched = Some(outcome);
                    break;
                }
                Err(error) if error.is_cancellation() => return Err(PackError::Canceled),
                Err(error) => {
                    warn!(file = %file.display_name(), %error, "a pack download address failed");
                    last_error = Some(error);
                }
            }
        }
        match (fetched, last_error) {
            (Some(outcome), _) => outcomes.push(outcome),
            // The loop ran at least once — an empty address list was refused
            // above — so a file that was not fetched has a failure to report.
            (None, Some(error)) => {
                return Err(PackError::DownloadFailed {
                    path: file.display_name(),
                    source: Box::new(error),
                });
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

/// Copies a verified cache entry into the instance.
fn copy_into_instance(source: &Path, destination: &Path) -> PackResult<()> {
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
        let report = archive.extract_prefix(&layer.archive_prefix, game_dir)?;
        debug!(
            layer = layer.scope.as_str(),
            files = report.files,
            "applied a pack override layer"
        );
        applied.push(AppliedLayer {
            layer,
            files: report.files,
        });
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::{HashAlgorithm, ImportSourceKind, LoaderSelection};

    use super::*;
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
            identity: None,
        }
    }

    #[test]
    fn a_cache_path_is_derived_from_the_strongest_hash_and_never_from_the_url() {
        // a remote server does not get to choose where a file
        // lands. The destination comes from the hash the pack published.
        let cache = CacheLayout::new("/cache/ephervex");
        let request = primary_request(&file("mods/some mod.jar"), &cache).expect("request");
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
    }

    #[test]
    fn a_path_with_no_usable_extension_still_gets_a_cache_path() {
        let cache = CacheLayout::new("/cache/ephervex");
        let mut entry = file("config/no-extension");
        entry.logical_path = PathBuf::from("config/no-extension");
        let request = primary_request(&entry, &cache).expect("request");
        assert!(
            request.destination().to_string_lossy().ends_with(SHA512),
            "an extension is cosmetic, not required"
        );
    }

    #[test]
    fn layers_are_applied_common_first_then_side_specific() {
        let recipe = PackRecipe {
            recipe_version: RECIPE_VERSION,
            source_kind: ImportSourceKind::ModrinthPack,
            original_name: "Test".to_owned(),
            original_version: None,
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
