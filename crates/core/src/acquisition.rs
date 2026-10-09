//! Releases this launcher is not permitted to download for itself.
//!
//! A CurseForge project's owner can turn third-party distribution off. The
//! release is still real, still described, and still the one that fits the
//! instance (see [`ephervex_mod_platform::Distribution`]); only its bytes are
//! out of reach. So the restriction is handled where bytes are actually
//! needed, and nowhere else:
//!
//! * a verified copy already in the shared artifact cache installs the
//!   ordinary way, with no address and no browser;
//! * a copy that is nowhere yet turns the operation into the same
//!   user-assisted acquisition a pack import already offers - the project's
//!   own page on the provider's site, the exact file name, and the digest a
//!   hand-placed copy is recognized by.
//!
//! No delivery address is ever assembled out of the ids. That would route
//! around exactly the decision the author made.

use std::path::Path;

use ephervex_download::OperationReporter;
use ephervex_download::{ContentStore, FileExtension, file_matches_hashes};
use ephervex_mod_platform::{CurseForgeProjectClass, ModInstallPlan, PlannedMod, file_browser_url};
use ephervex_model::{CurseForgeFileId, Hash, ModProvider};
use ephervex_pack::{
    BlockedProviderFile, PackRecipe, ProviderFileIdentity, SideRequirement, safe_file_name,
};
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::error::{CoreError, CoreResult};

impl App {
    /// Whether the shared artifact cache already holds a verified copy of
    /// these bytes.
    ///
    /// Proof, not a guess: the content-addressed path is derived from the
    /// strongest published digest and its contents are hashed again before
    /// the answer is yes. `file_name` is read only for its cosmetic
    /// extension, which is what the cache layout appends; it never names a
    /// destination.
    ///
    /// The verified copy's size, or `None` for a file with no published
    /// digest as well as for a genuine miss - either way there is nothing a
    /// hand-placed copy could have been verified against.
    pub(crate) async fn cached_artifact_bytes(
        &self,
        hashes: &[Hash],
        file_name: Option<&str>,
    ) -> CoreResult<Option<u64>> {
        let Some(hash) = hashes.first() else {
            return Ok(None);
        };
        let extension = file_name
            .and_then(safe_file_name)
            .and_then(|name| {
                name.extension()
                    .and_then(|extension| extension.to_str())
                    .map(ToOwned::to_owned)
            })
            .and_then(|extension| FileExtension::new(&extension).ok());
        let path = self.artifact_cache().layout().content(
            ContentStore::Downloads,
            hash,
            extension.as_ref(),
        );
        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(CoreError::from(ephervex_pack::PackError::DownloadFailed {
                    path: file_name.unwrap_or_default().to_owned(),
                    source: Box::new(ephervex_download::DownloadError::io(
                        ephervex_download::ArtifactStep::ReadCached,
                        &path,
                        source,
                    )),
                }));
            }
        };
        if !file_matches_hashes(&path, hashes).await.map_err(|source| {
            CoreError::from(ephervex_pack::PackError::DownloadFailed {
                path: file_name.unwrap_or_default().to_owned(),
                source: Box::new(source),
            })
        })? {
            return Ok(None);
        }
        Ok(Some(metadata.len()))
    }

    /// The planned mods whose bytes this launcher may not fetch and does not
    /// already have.
    ///
    /// A restricted release the cache has verified, or whose jar is already
    /// sitting at its destination in the instance, is not in the answer: the
    /// restriction is about downloading, not about using bytes this machine
    /// legitimately holds.
    pub(crate) async fn unacquirable(
        &self,
        plan: &ModInstallPlan,
        game_dir: &Path,
    ) -> CoreResult<Vec<BlockedProviderFile>> {
        let mut blocked = Vec::new();
        for planned in plan.all_mods() {
            if !planned.version.file.distribution.is_restricted() {
                continue;
            }
            let file_name = planned.version.file.file_name.as_str();
            let hashes = &planned.version.file.hashes;
            if self
                .cached_artifact_bytes(hashes, Some(file_name))
                .await?
                .is_some()
            {
                continue;
            }
            if installed_copy_matches(game_dir, file_name, hashes).await {
                continue;
            }
            blocked.push(blocked_file(planned, plan.provider));
        }
        Ok(blocked)
    }

    /// Turns a failure that was really "nobody may fetch these bytes" into the
    /// refusal that says so, and leaves every other failure alone.
    ///
    /// The pack layer is what discovers this: it decides per file whether
    /// bytes are needed at all, probes the cache, and only then reports a file
    /// it has no address for. Re-deriving that decision here would be a second
    /// implementation of it, so this runs on the failure it produced.
    pub(crate) async fn as_manual_acquisition(
        &self,
        error: CoreError,
        target: &Path,
        plan: &ModInstallPlan,
        game_dir: &Path,
    ) -> CoreError {
        if !matches!(
            error,
            CoreError::Pack(ephervex_pack::PackError::FileDownloadsMissing { .. })
        ) {
            return error;
        }
        match self.unacquirable(plan, game_dir).await {
            Ok(blocked) if !blocked.is_empty() => CoreError::ModAcquisitionRestricted {
                path: target.to_path_buf(),
                blocked,
            },
            // The address was missing for some other reason, or reading the
            // cache failed in turn. Either way the original failure is the
            // honest one to report.
            Ok(_) | Err(_) => error,
        }
    }
}

impl App {
    /// The same refusal, for an operation that works from a recorded entry
    /// rather than from a resolved plan.
    ///
    /// Putting a deleted jar back and reconstructing a linked server both need
    /// bytes from retained desired state. A recipe entry recording a restricted
    /// release carries no address to fetch them from. What the entry does not
    /// carry is the project's name and handle, so those are asked for once,
    /// here, on the failure path. Materialization itself still asks no catalog.
    pub(crate) async fn as_recorded_manual_acquisition(
        &self,
        error: CoreError,
        recipe: &PackRecipe,
        logical_path: &Path,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreError {
        if !matches!(
            error,
            CoreError::Pack(ephervex_pack::PackError::FileDownloadsMissing { .. })
        ) {
            return error;
        }
        let Some(entry) = recipe
            .files
            .iter()
            .find(|file| file.logical_path == logical_path)
        else {
            return error;
        };
        // A pack file whose mirror is simply gone has no page to send anyone
        // to and no digest to recognize a copy by. That is the failure it
        // already is.
        let Some(identity) = entry.identity.as_ref() else {
            return error;
        };
        let (Some(provider), Some(project_id)) =
            (identity.provider, identity.project_id.as_deref())
        else {
            return error;
        };
        if entry.hashes.is_empty() || !entry.downloads.is_empty() {
            return error;
        }

        let project = match self.mod_provider(provider) {
            Ok(catalog) => catalog.project(project_id, reporter, cancel).await.ok(),
            Err(_) => None,
        };
        let version_id = identity.version_id.as_deref().unwrap_or_default();
        let browser_url = browser_url(
            provider,
            project.as_ref().and_then(|it| it.slug.as_deref()),
            version_id,
        );
        if browser_url.is_none() && !matches!(provider, ModProvider::CurseForge) {
            // Nothing this build knows restricts downloads, so this is an
            // ordinary missing source rather than an author's decision.
            return error;
        }

        CoreError::ModAcquisitionRestricted {
            path: logical_path.to_path_buf(),
            blocked: vec![BlockedProviderFile {
                identity: identity.clone(),
                project_name: project.map(|it| it.title),
                file_name: entry
                    .logical_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
                browser_url,
                hashes: entry.hashes.clone(),
                relations: entry.relations.clone(),
                client: entry.client,
                server: entry.server,
            }],
        }
    }
}

/// Whether the instance already has this exact file at its destination.
///
/// A restricted mod that is installed and unchanged needs nothing fetched, so
/// asking the user to fetch it again would be asking for a file they have.
async fn installed_copy_matches(game_dir: &Path, file_name: &str, hashes: &[Hash]) -> bool {
    let Some(name) = safe_file_name(file_name) else {
        return false;
    };
    let path = game_dir.join(ephervex_pack::MODS_DIR).join(name);
    if !path.is_file() {
        return false;
    }
    file_matches_hashes(&path, hashes).await.unwrap_or(false)
}

/// One planned mod as something the user can be asked to fetch by hand.
///
/// The same shape a blocked pack-import file has, so both go through one
/// presentation, one folder watcher, and one set of identity rules.
fn blocked_file(planned: &PlannedMod, provider: ModProvider) -> BlockedProviderFile {
    let version = &planned.version;
    let file_id = match provider {
        ModProvider::CurseForge => Some(version.id.clone()),
        _ => None,
    };
    BlockedProviderFile {
        identity: ProviderFileIdentity {
            provider: Some(provider),
            project_id: Some(version.project_id.clone()),
            version_id: Some(version.id.clone()),
            file_id: file_id.clone(),
        },
        project_name: Some(planned.project.title.clone()),
        file_name: Some(version.file.file_name.clone()),
        browser_url: browser_url(provider, planned.project.slug.as_deref(), &version.id),
        hashes: version.file.hashes.clone(),
        relations: Some(crate::mods::recipe_relations(&version.dependencies)),
        // The user asked for this mod on this client, which is a fact about
        // the instance rather than a claim about the project.
        client: SideRequirement::Required,
        server: crate::mods::side_requirement(planned.server_side()),
    }
}

/// The page on the provider's own site that offers this exact file.
///
/// `None` where one cannot be built without guessing - a CurseForge project
/// whose slug is unknown, or a provider with no restricted files to send
/// anyone to in the first place.
fn browser_url(provider: ModProvider, slug: Option<&str>, version_id: &str) -> Option<String> {
    match provider {
        ModProvider::CurseForge => {
            let segment = CurseForgeProjectClass::Mods.browser_path_segment()?;
            let file = CurseForgeFileId::parse(version_id)?;
            Some(file_browser_url(segment, slug?, file))
        }
        // Nothing else this build talks to restricts third-party downloads,
        // so nothing else has a page to send a user to for one.
        _ => None,
    }
}
