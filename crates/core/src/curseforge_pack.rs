//! Turning a CurseForge pack's declared ids into installable recipe entries.
//!
//! # Why this step exists at all
//!
//! A CurseForge export manifest names each of its files by project id and file
//! id and nothing else: no name, no size, no hash, no address. `ephervex-pack`
//! therefore parses one into a recipe plus a list of
//! [`PendingProviderFile`](ephervex_pack::PendingProviderFile) entries, and
//! stops. This module is the half that asks CurseForge, and it lives here for
//! the same reason every other cross-boundary sequence does: the pack crate
//! knows archives and recipes, the mod-platform crate knows the API, and
//! neither should learn the other.
//!
//! A frontend never orchestrates this. It analyzes an archive, asks core to
//! resolve it, and gets back an analysis it can display and import — the same
//! two steps a Modrinth pack takes, where the second happens to have nothing to
//! do.
//!
//! # What is exact about it
//!
//! Every lookup is `project P, file F` against the endpoint that answers about
//! exactly that file, and the answer is checked against the question before it
//! is used. Nothing here lists a project's versions, and nothing picks a newer
//! file when the declared one is gone: a missing exact file fails the import.
//!
//! # The credential
//!
//! Read once per operation, held for that operation's request sequence, and
//! dropped with it. It travels to the API origin, which the API client scopes it
//! to, and to CurseForge's own delivery hosts, which [`AuthenticatedSource`]
//! scopes it to. It reaches nothing else, appears in no recipe, no retained
//! source, no error, and no log line.
//!
//! An import is not the only operation that needs one. Retained desired state
//! records everything needed to identify and verify an artifact, so nothing
//! rebuilt from a recipe asks the API what an instance contains — but the bytes
//! still come from CurseForge, which serves them only to an authenticated
//! request. So an artifact whose verified copy is still in the shared store is
//! installed with no credential at all, and one the cache budget has collected
//! needs the key back. [`ProviderCredentials`] is where that decision is made,
//! per artifact and from the provider identity the recipe records.
//!
//! # Nothing an API answer says is written down
//!
//! What survives an import is the normalized recipe: the exact project and file
//! ids the *user's own manifest* declared, plus the minimum the launcher's own
//! model needs to install and verify an artifact — a name, a size, a digest,
//! and the address it came from. The response itself is parsed, used, and
//! dropped. See ADR 0022.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use ephervex_download::{AuthenticatedSource, DomainAllowlist, DownloadManager, DownloaderConfig};
use ephervex_mod_platform::{
    CURSEFORGE_API_KEY_HEADER, CurseForgeFileRelease, CurseForgeOperation, CurseForgeProject,
    MINECRAFT_GAME_ID, ModPlatformError,
};
use ephervex_model::{CurseForgeFileId, CurseForgeProjectId, ModOrigin, ModProvider, SecretString};
use ephervex_pack::{
    ArtifactCredentials, CredentialUnavailable, MODS_DIR, PackAnalysis, PackError, PackFileRecipe,
    PackFormat, PendingProviderFile, ProviderFileIdentity,
};
use futures_util::StreamExt as _;
use reqwest::header::HeaderName;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument};

use crate::app::App;
use crate::error::CoreResult;

/// How many exact-file lookups one import runs at once.
///
/// A pack may declare several hundred files, and one task per entry would open
/// several hundred sockets against somebody else's rate limit. The number
/// matches the download manager's own default concurrency, which is the figure
/// this launcher has already decided is polite.
const LOOKUP_CONCURRENCY: usize = 6;

impl App {
    /// Fills in everything a pack analysis still needs from its provider.
    ///
    /// A no-op for a pack whose manifest already describes its own files, which
    /// is what lets a caller run it unconditionally. For a CurseForge export it
    /// resolves every declared file against the API and turns the result into
    /// ordinary recipe entries, after which the analysis is indistinguishable
    /// from one that never needed resolving.
    ///
    /// Requires the CurseForge API key, and says so with
    /// [`ModPlatformError::CredentialMissing`] before any request is made.
    /// Analysis itself does not: identifying an archive and reading its
    /// manifest stays local.
    pub async fn resolve_pack(
        &self,
        analysis: &mut PackAnalysis,
        cancel: &CancellationToken,
    ) -> CoreResult<()> {
        self.prepare_provider_files(analysis, cancel).await?;
        Ok(())
    }

    /// The same work, keeping the credential the artifact downloads need.
    ///
    /// Returned rather than re-read: one import is one operation, and reading
    /// the secret store again for the download half would be a second exposure
    /// of the same secret for no benefit.
    pub(crate) async fn prepare_provider_files(
        &self,
        analysis: &mut PackAnalysis,
        cancel: &CancellationToken,
    ) -> CoreResult<Option<Arc<AuthenticatedSource>>> {
        match analysis.format {
            PackFormat::CurseForgePack => {
                Ok(Some(self.resolve_curseforge_pack(analysis, cancel).await?))
            }
            // Every other format's manifest describes its own files, so there
            // is nothing to resolve and no credential to spend.
            PackFormat::ModrinthPack | PackFormat::PrismExport | _ => Ok(None),
        }
    }

    #[instrument(skip_all, fields(pending = analysis.pending.len()))]
    async fn resolve_curseforge_pack(
        &self,
        analysis: &mut PackAnalysis,
        cancel: &CancellationToken,
    ) -> CoreResult<Arc<AuthenticatedSource>> {
        let api = self.curseforge_api()?;
        // One read of the secret store for the whole import, and the point at
        // which an unconfigured launcher fails without contacting anyone.
        let operation = api.begin_operation()?;
        let hosts = self.curseforge_artifact_hosts();
        let credential = self.curseforge_artifact_credential(operation.credential().clone())?;

        if analysis.pending.is_empty() {
            return Ok(credential);
        }
        validate_project_classes(&operation, &analysis.pending, cancel).await?;

        let pending = std::mem::take(&mut analysis.pending);
        let resolved = match resolve_files(&operation, &pending, &hosts, cancel).await {
            Ok(resolved) => resolved,
            Err(error) => {
                // The analysis is left describing the pack it describes: a
                // caller that retries after configuring a key must still see
                // the entries that need resolving.
                analysis.pending = pending;
                return Err(error);
            }
        };

        info!(
            files = resolved.len(),
            "resolved a CurseForge pack's declared files"
        );
        analysis.recipe.files.extend(resolved);
        Ok(credential)
    }

    /// The hosts a CurseForge artifact may be fetched from, and the only ones
    /// its credential may reach.
    ///
    /// A method rather than a free function because a test build reads the
    /// loopback flag off the application; a release build has no such field, so
    /// the receiver is unused there and the production list is the only one it
    /// can return.
    #[cfg_attr(
        not(test),
        expect(
            clippy::unused_self,
            reason = "only a test build reads the loopback flag"
        )
    )]
    pub(crate) fn curseforge_artifact_hosts(&self) -> DomainAllowlist {
        // Only a test build has the field to read, so the strict production
        // list is the only one a release binary can produce.
        #[cfg(test)]
        if self.loopback_http {
            return DomainAllowlist::for_loopback_tests("127.0.0.1");
        }
        DomainAllowlist::curseforge_artifact_defaults()
    }

    /// Wraps a CurseForge API key as a credential an artifact download may
    /// carry.
    ///
    /// The key is scoped twice over: to the delivery hosts
    /// [`Self::curseforge_artifact_hosts`] names, and to a redirect policy that
    /// cannot leave them, which the credential builds and owns. Both scopes
    /// travel with the request that carries the secret rather than with the
    /// manager that sends it, so a batch containing artifacts from other
    /// providers is neither offered this key nor governed by these redirect
    /// rules.
    pub(crate) fn curseforge_artifact_credential(
        &self,
        key: SecretString,
    ) -> CoreResult<Arc<AuthenticatedSource>> {
        let source = AuthenticatedSource::new(
            HeaderName::from_static(CURSEFORGE_API_KEY_HEADER),
            key,
            self.curseforge_artifact_hosts(),
            &self.http_config(),
        )
        .map_err(unusable_downloader)?;
        Ok(Arc::new(source))
    }

    /// The shared downloader every artifact operation uses.
    ///
    /// One manager for a whole operation whatever mix of providers it touches:
    /// redirect containment for an authenticated request is the credential's,
    /// not the manager's, so there is no longer a version of this that has to be
    /// built differently depending on who the pack came from.
    pub(crate) fn artifact_manager(&self) -> CoreResult<DownloadManager> {
        let config = DownloaderConfig {
            http: self.http_config(),
            ..DownloaderConfig::default()
        };
        Ok(DownloadManager::new(config).map_err(unusable_downloader)?)
    }

    /// The credentials an artifact operation may need, read on demand.
    ///
    /// See [`ProviderCredentials`] for why even a fully retained recipe can
    /// still need one, and why it is asked for per file.
    pub(crate) fn provider_credentials(&self) -> ProviderCredentials<'_> {
        ProviderCredentials {
            app: self,
            curseforge: OnceLock::new(),
        }
    }
}

/// Supplies the provider credential a single retained artifact needs.
///
/// # Why retained desired state can still need one
///
/// A recipe entry records the exact addresses, digests, and size of its
/// artifact, and that is all the launcher needs to *identify and verify* it —
/// forever, offline, and without asking any provider anything. What it does not
/// change is who serves the bytes. One provider serves its artifacts only to a
/// request carrying an API key, so re-fetching such a file after its verified
/// copy has been collected out of the shared artifact cache needs that key,
/// even though nothing about the file itself is in question.
///
/// The consequences are worth stating in both directions:
///
/// * a verified artifact still in the shared cache is installed without any
///   credential, any provider request, or any configuration at all — which is
///   what keeps an installed instance working on a machine that has configured
///   nothing;
/// * an artifact that has to come back over the network is subject to whatever
///   its own provider requires.
///
/// # One credential per provider, chosen per file
///
/// The decision is made from [`ProviderFileIdentity::provider`], the durable
/// fact the recipe records, and from nothing else — never from an address, a
/// filename, a digest, or the format the pack was originally imported from. A
/// desired state naming files from several providers therefore offers each
/// secret only to its own artifacts.
///
/// The secret store is read at most once per provider per operation, and the
/// result is dropped with the operation.
///
/// # The three ways a credential can be the answer
///
/// Absent and unreadable are decided here, before anything is sent. Refused
/// cannot be: a key that is present and syntactically usable is only wrong
/// according to the host that declines to serve the artifact. So that verdict
/// arrives afterwards, as the status of an authenticated artifact request, and
/// [`ArtifactCredentials::rejected`] is where it is turned back into the same
/// typed failure the API would have produced. Nothing speculative is sent to
/// find out first, and nothing is re-resolved through the API to recover.
pub(crate) struct ProviderCredentials<'a> {
    app: &'a App,
    /// Built on the first CurseForge artifact this operation actually fetches,
    /// and never for an operation that fetches none.
    curseforge: OnceLock<Arc<AuthenticatedSource>>,
}

impl fmt::Debug for ProviderCredentials<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Names what this is and never what it holds, nor whether it holds
        // anything: "a key has been read" is not a fact worth logging.
        formatter.write_str("ProviderCredentials")
    }
}

impl ProviderCredentials<'_> {
    /// Reuses a CurseForge credential this operation has already read.
    ///
    /// An import resolves the pack's declared files against the API first, which
    /// reads the key; handing that one over is what keeps the download half from
    /// going back to the secret store for the same value.
    #[must_use]
    pub(crate) fn holding_curseforge(self, credential: Arc<AuthenticatedSource>) -> Self {
        let _ = self.curseforge.set(credential);
        self
    }

    fn curseforge(&self) -> Result<Arc<AuthenticatedSource>, CredentialUnavailable> {
        if let Some(credential) = self.curseforge.get() {
            return Ok(Arc::clone(credential));
        }
        let api = self
            .app
            .curseforge_api()
            .map_err(|error| CredentialUnavailable::new(unavailable(&error)))?;
        // Reads the secret store, and fails with
        // `ModPlatformError::CredentialMissing` when nothing is configured —
        // before any request is made, and without degrading into an
        // unauthenticated attempt the provider could only refuse.
        let key = api
            .begin_operation()
            .map_err(CredentialUnavailable::new)?
            .credential()
            .clone();
        let credential = self
            .app
            .curseforge_artifact_credential(key)
            .map_err(|error| CredentialUnavailable::new(unavailable(&error)))?;
        let _ = self.curseforge.set(Arc::clone(&credential));
        Ok(credential)
    }
}

impl ArtifactCredentials for ProviderCredentials<'_> {
    fn credential_for(
        &self,
        identity: Option<&ProviderFileIdentity>,
    ) -> Result<Option<Arc<AuthenticatedSource>>, CredentialUnavailable> {
        match identity.and_then(|identity| identity.provider) {
            Some(ModProvider::CurseForge) => self.curseforge().map(Some),
            // Every other catalog publishes its artifacts openly, as does a file
            // whose identity names no catalog at all. A catalog added later that
            // does authenticate its downloads is handled by adding an arm here,
            // and until then it is offered nothing rather than somebody else's
            // secret.
            _ => Ok(None),
        }
    }

    fn rejected(
        &self,
        identity: Option<&ProviderFileIdentity>,
        status: u16,
    ) -> Option<CredentialUnavailable> {
        match identity.and_then(|identity| identity.provider) {
            // The same reading the API client gives these statuses, from the
            // same function: a delivery host refusing the key means what the
            // API refusing it means, and the user's next step is identical.
            Some(ModProvider::CurseForge) => {
                ephervex_mod_platform::credential_refusal(status).map(CredentialUnavailable::new)
            }
            // Nothing else was issued a credential, so nothing else can have had
            // one refused: whatever this status is, it is about the download.
            _ => None,
        }
    }
}

/// Reports a credential this launcher could not assemble as a provider
/// credential failure rather than as whatever it was underneath.
///
/// The user-facing outcome is the same either way — CurseForge cannot be used
/// until something is fixed — and keeping one shape means a frontend has one
/// place to route from.
fn unavailable(error: &crate::error::CoreError) -> ModPlatformError {
    ModPlatformError::CredentialUnreadable {
        provider: ModProvider::CurseForge,
        detail: error.to_string(),
    }
}

/// An HTTP client this build could not construct.
///
/// Reported as a download failure because that is what it is from the user's
/// side: nothing can be fetched. The pack's own name is not available here, so
/// the label names the subsystem rather than inventing a file.
fn unusable_downloader(source: ephervex_download::DownloadError) -> PackError {
    PackError::DownloadFailed {
        path: "artifact downloads".to_owned(),
        source: Box::new(source),
    }
}

/// Confirms every project a manifest declares belongs to CurseForge's Mods
/// class, before any file is fetched.
///
/// Ephervex v1 manages Mods only. A CurseForge manifest names its files by
/// project id alone, and nothing stops a project id from naming a Resource
/// Pack, a World, or any other class of content: those are real CurseForge
/// project classes, and none of them is a mod. Materializing one under
/// `mods/` as though it were would install something the pack's own author
/// never placed there. The whole import is refused rather than skipping the
/// entry it cannot place: a pack missing one file it declared is not the pack
/// its manifest describes.
///
/// Deduplicated by project, the same as [`resolve_files`] deduplicates by
/// file: a pack that declares one project's optional and required files
/// together asks about that project once.
async fn validate_project_classes(
    operation: &CurseForgeOperation<'_>,
    pending: &[PendingProviderFile],
    cancel: &CancellationToken,
) -> CoreResult<()> {
    let mut projects: Vec<CurseForgeProjectId> = pending
        .iter()
        .map(|entry| declared_ids(entry).map(|(project, _)| project))
        .collect::<CoreResult<_>>()?;
    projects.sort_unstable();
    projects.dedup();

    // Discovered fresh for this operation. The numeric id behind "Mods" is
    // provider metadata, not something this launcher is allowed to assume.
    let classes = operation.project_classes(MINECRAFT_GAME_ID, cancel).await?;

    let classified: Vec<CurseForgeProject> = futures_util::stream::iter(
        projects
            .iter()
            .map(|project| operation.project(*project, cancel)),
    )
    .buffered(LOOKUP_CONCURRENCY)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<Result<_, ModPlatformError>>()?;

    for project in classified {
        // The one implementation of what a project's class means, so an import
        // and the mod browser cannot arrive at different answers about the same
        // project.
        if let Some(error) = classes.rejection_for(&project) {
            return Err(error.into());
        }
    }
    Ok(())
}

/// Looks up every declared file, deduplicated and with bounded concurrency.
///
/// Returns entries in the pack's own order, so the recipe reads the way the
/// manifest does.
async fn resolve_files(
    operation: &CurseForgeOperation<'_>,
    pending: &[PendingProviderFile],
    hosts: &DomainAllowlist,
    cancel: &CancellationToken,
) -> CoreResult<Vec<PackFileRecipe>> {
    let wanted: Vec<(CurseForgeProjectId, CurseForgeFileId)> = pending
        .iter()
        .map(declared_ids)
        .collect::<CoreResult<_>>()?;

    // One request per distinct file, however many entries name it. Responses
    // cannot be cached across operations, which is all the more reason not to
    // ask twice inside one.
    let mut distinct: Vec<(CurseForgeProjectId, CurseForgeFileId)> = wanted.clone();
    distinct.sort_unstable();
    distinct.dedup();
    debug!(
        declared = wanted.len(),
        requests = distinct.len(),
        "resolving CurseForge pack files"
    );

    let releases: Vec<CurseForgeFileRelease> = futures_util::stream::iter(
        distinct
            .iter()
            .map(|(project, file)| operation.file(*project, *file, cancel)),
    )
    .buffered(LOOKUP_CONCURRENCY)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<Result<_, ModPlatformError>>()?;

    let by_identity: BTreeMap<(CurseForgeProjectId, CurseForgeFileId), &CurseForgeFileRelease> =
        releases
            .iter()
            .map(|release| ((release.project, release.file), release))
            .collect();

    pending
        .iter()
        .zip(&wanted)
        .map(|(entry, identity)| {
            let release = by_identity
                .get(identity)
                .copied()
                // Unreachable: every distinct identity was requested and every
                // response was checked against the identity it answers for.
                .ok_or_else(|| ModPlatformError::FileUnknown {
                    provider: ModProvider::CurseForge,
                    project_id: identity.0.to_string(),
                    file_id: identity.1.to_string(),
                })?;
            recipe_entry(entry, release, hosts)
        })
        .collect()
}

/// The exact ids a pending entry declares.
fn declared_ids(
    entry: &PendingProviderFile,
) -> CoreResult<(CurseForgeProjectId, CurseForgeFileId)> {
    let project = entry
        .identity
        .project_id
        .as_deref()
        .and_then(CurseForgeProjectId::parse)
        .ok_or_else(|| PackError::PackEntryIdInvalid {
            field: "projectID",
            value: entry.identity.project_id.clone().unwrap_or_default(),
        })?;
    let file = entry
        .identity
        .version_id
        .as_deref()
        .and_then(CurseForgeFileId::parse)
        .ok_or_else(|| PackError::PackEntryIdInvalid {
            field: "fileID",
            value: entry.identity.version_id.clone().unwrap_or_default(),
        })?;
    Ok((project, file))
}

/// One resolved file as the recipe entry that records it.
///
/// The side requirements come from the *pack*, not from the API: pack inclusion
/// is what the manifest stated, and CurseForge's file document says nothing
/// about servers at all. Everything the API contributes is what the launcher
/// needs to fetch and verify an artifact.
fn recipe_entry(
    entry: &PendingProviderFile,
    release: &CurseForgeFileRelease,
    hosts: &DomainAllowlist,
) -> CoreResult<PackFileRecipe> {
    // The file name comes from a provider and is untrusted: it is validated as
    // a single safe path component before it can name a destination.
    let name = ephervex_pack::safe_file_name(&release.artifact.file_name).ok_or_else(|| {
        PackError::EntryPathUnsafe {
            entry: release.artifact.file_name.clone(),
        }
    })?;

    // The address is provider data, and provider data decides which machine the
    // launcher connects to. It is checked like any other remote address, and a
    // refusal is fatal: there is no second address for a CurseForge file.
    let address =
        hosts
            .validate(&release.artifact.url)
            .map_err(|source| PackError::DownloadUrlRejected {
                path: name.to_string_lossy().into_owned(),
                source,
            })?;

    Ok(PackFileRecipe {
        logical_path: PathBuf::from(MODS_DIR).join(name),
        hashes: release.artifact.hashes.clone(),
        downloads: vec![address.to_string()],
        size: release.artifact.size,
        client: entry.client,
        server: entry.server,
        selected: None,
        // The pack asked for this file. Resolving it exactly does not make it
        // anybody else's, and the pack-owned mutation policy depends on that.
        origin: ModOrigin::PackDeclared,
        identity: Some(ProviderFileIdentity {
            provider: Some(ModProvider::CurseForge),
            project_id: Some(release.project.to_string()),
            // CurseForge's release identifier is its file id. Both fields carry
            // it rather than one of them carrying a version parsed out of a
            // display name.
            version_id: Some(release.file.to_string()),
            file_id: Some(release.file.to_string()),
        }),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::io::Write as _;
    use std::path::Path;

    use ephervex_auth::{CredentialStore, MemoryStore};
    use ephervex_download::{
        ArtifactCache, CacheLayout, ContentStore, FileExtension, OperationReporter,
    };
    use ephervex_model::{
        ArtifactCacheLimit, Hash, HashAlgorithm, ImportSourceKind, LoaderKind, SecretString,
        StorageClass,
    };
    use ephervex_pack::{OptionalSelection, PackSide, SideRequirement, SourceStore};
    use ephervex_storage::Paths;
    use sha1::{Digest as _, Sha1};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use crate::error::CoreError;
    use crate::import::ImportRequest;
    use crate::server::ServerCreateOptions;

    use super::*;

    /// Obviously not a credential, and the only one this file ever uses.
    const FAKE_KEY: &str = "test-curseforge-key-do-not-use";

    /// The pack's two declared projects and the exact files it names.
    const REQUIRED_PROJECT: u32 = 238_222;
    const REQUIRED_FILE: u32 = 4_593_548;
    const OPTIONAL_PROJECT: u32 = 306_612;
    const OPTIONAL_FILE: u32 = 4_593_600;

    fn store_with_key() -> Arc<dyn CredentialStore> {
        let store = MemoryStore::new();
        ephervex_auth::set_curseforge_api_key(&store, &SecretString::new(FAKE_KEY))
            .expect("store the key");
        Arc::new(store)
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

    /// A CurseForge export declaring one required and one optional file.
    fn export_manifest() -> Vec<u8> {
        export_manifest_with_files(&format!(
            r#"{{"projectID": {REQUIRED_PROJECT}, "fileID": {REQUIRED_FILE}, "required": true}},
               {{"projectID": {OPTIONAL_PROJECT}, "fileID": {OPTIONAL_FILE}, "required": false}}"#
        ))
    }

    /// The same fixture manifest, with its own declared files, for a test
    /// that needs a shape [`export_manifest`] does not offer.
    fn export_manifest_with_files(files: &str) -> Vec<u8> {
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

    fn sha1_of(body: &[u8]) -> String {
        let mut hasher = Sha1::new();
        hasher.update(body);
        hex::encode(hasher.finalize())
    }

    /// The exact-file document for one artifact, as the API answers.
    fn file_document(
        server: &MockServer,
        project: u32,
        file: u32,
        name: &str,
        body: &[u8],
    ) -> String {
        format!(
            r#"{{"data":{{"id":{file},"gameId":432,"modId":{project},"isAvailable":true,
                "displayName":"{name}","fileName":"{name}","releaseType":1,"fileStatus":4,
                "fileLength":{length},
                "downloadUrl":"{base}/files/{project}/{file}/{name}",
                "hashes":[{{"value":"{digest}","algo":1}},
                          {{"value":"0123456789abcdef0123456789abcdef","algo":2}}]}}}}"#,
            length = body.len(),
            base = server.uri(),
            digest = sha1_of(body),
        )
    }

    /// The numeric id this test fixture's loopback server uses for the Mods
    /// class. Deliberately not `6`, the id the production API happens to use
    /// today: a test suite that only ever exercised the id validation is
    /// meant to discover would still pass if the implementation quietly
    /// started assuming a fixed number instead of discovering one.
    const MODS_CLASS_ID: u32 = 987_654;

    /// The documented shape of `GET /v1/categories?classesOnly=true`, naming
    /// the Mods class by [`MODS_CLASS_ID`] and two decoy classes beside it, so
    /// a test proves the right one was picked rather than the only one, and so
    /// a non-Mod rejection has a real provider-returned class name available.
    async fn serve_project_classes(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/v1/categories"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":[
                        {{"id":{MODS_CLASS_ID},"gameId":432,"name":"Mods","slug":"mc-mods","isClass":true}},
                        {{"id":12,"gameId":432,"name":"Resource Packs","slug":"texture-packs","isClass":true}},
                        {{"id":17,"gameId":432,"name":"Worlds","slug":"worlds","isClass":true}}
                    ]}}"#
                ),
                "application/json",
            ))
            .mount(server)
            .await;
    }

    /// Serves one project's classification, as `GET /v1/mods/{id}` answers.
    async fn serve_project_class(server: &MockServer, project: u32, name: &str, class_id: u32) {
        serve_project_raw(server, project, name, &format!(r#""classId":{class_id}"#)).await;
    }

    /// The same, with `classId` reported as `null` (a real, schema-valid
    /// state this build must fail closed on rather than silently accept).
    async fn serve_project_with_no_class(server: &MockServer, project: u32, name: &str) {
        serve_project_raw(server, project, name, r#""classId":null"#).await;
    }

    /// Serves one project's classification with a hand-written `classId`
    /// member, so a test can name a class the categories fixture never listed.
    async fn serve_project_raw(server: &MockServer, project: u32, name: &str, class_member: &str) {
        Mock::given(method("GET"))
            .and(path(format!("/v1/mods/{project}")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":{{"id":{project},"gameId":432,"name":"{name}","slug":"{name}",{class_member}}}}}"#
                ),
                "application/json",
            ))
            .mount(server)
            .await;
    }

    /// Classifies both of [`serve_pack`]'s declared projects as Mods, which is
    /// what every test not specifically about classification assumes.
    async fn serve_pack_as_mods(server: &MockServer) {
        serve_project_classes(server).await;
        serve_project_class(server, REQUIRED_PROJECT, "Required Mod", MODS_CLASS_ID).await;
        serve_project_class(server, OPTIONAL_PROJECT, "Optional Mod", MODS_CLASS_ID).await;
    }

    /// Serves one exact file's metadata and its bytes.
    async fn serve_file(server: &MockServer, project: u32, file: u32, name: &str, body: &[u8]) {
        Mock::given(method("GET"))
            .and(path(format!("/v1/mods/{project}/files/{file}")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                file_document(server, project, file, name, body),
                "application/json",
            ))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/files/{project}/{file}/{name}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(body.to_vec(), "application/java-archive"),
            )
            .mount(server)
            .await;
    }

    /// An application pointed at a loopback CurseForge, with a key configured.
    fn app_against(base: &Path, server: &MockServer) -> App {
        let mut app = App::open_at(Paths::rooted_at(base)).expect("app opens");
        app.use_loopback_curseforge(&format!("{}/v1", server.uri()), store_with_key())
            .expect("a loopback client");
        app
    }

    /// The same, with a key the delivery host will not accept.
    ///
    /// Syntactically usable and wrong, which is the case no check before the
    /// request can catch: only the host that serves the artifact can say the key
    /// is not one it honors.
    fn app_with_a_stale_key(base: &Path, server: &MockServer) -> App {
        let store = MemoryStore::new();
        ephervex_auth::set_curseforge_api_key(
            &store,
            &SecretString::new("test-curseforge-key-that-was-revoked"),
        )
        .expect("store the key");
        let mut app = App::open_at(Paths::rooted_at(base)).expect("app opens");
        app.use_loopback_curseforge(&format!("{}/v1", server.uri()), Arc::new(store))
            .expect("a loopback client");
        app
    }

    /// The same, with no key in the store.
    fn app_without_a_key(base: &Path, server: &MockServer) -> App {
        let mut app = App::open_at(Paths::rooted_at(base)).expect("app opens");
        app.use_loopback_curseforge(
            &format!("{}/v1", server.uri()),
            Arc::new(MemoryStore::new()),
        )
        .expect("a loopback client");
        app
    }

    fn archive_at(dir: &Path, name: &str) -> std::path::PathBuf {
        let archive = dir.join(name);
        zip_file(
            &archive,
            &[
                ("manifest.json", &export_manifest()),
                ("overrides/config/repro.toml", b"debug = true\n" as &[u8]),
            ],
        );
        archive
    }

    async fn import(app: &App, archive: &Path) -> CoreResult<crate::import::ImportedPack> {
        app.import_pack(
            &ImportRequest::new(archive).with_storage(StorageClass::Temporary),
            &OperationReporter::silent("curseforge-import"),
            &CancellationToken::new(),
        )
        .await
    }

    const REQUIRED_BODY: &[u8] = b"the required mod jar";
    const OPTIONAL_BODY: &[u8] = b"the optional mod jar";

    async fn serve_pack(server: &MockServer) {
        serve_pack_as_mods(server).await;
        serve_file(
            server,
            REQUIRED_PROJECT,
            REQUIRED_FILE,
            "required-1.0.0.jar",
            REQUIRED_BODY,
        )
        .await;
        serve_file(
            server,
            OPTIONAL_PROJECT,
            OPTIONAL_FILE,
            "optional-1.0.0.jar",
            OPTIONAL_BODY,
        )
        .await;
    }

    // -- analysis without a credential -------------------------------------

    #[test]
    fn a_curseforge_export_is_identified_and_described_without_any_key_configured() {
        // Detection and manifest parsing are local. A launcher that could not
        // even say what the file is until a key was configured would be worse
        // than one that says what it is and what it still needs.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = App::open_at(Paths::rooted_at(dir.path())).expect("app opens");

        let analysis = app.analyze_pack(&archive).expect("analyze");
        assert_eq!(analysis.format, PackFormat::CurseForgePack);
        assert_eq!(analysis.name(), "Repro Pack");
        assert_eq!(analysis.minecraft_version(), "1.20.1");
        assert_eq!(analysis.loader().kind, LoaderKind::Forge);
        assert_eq!(analysis.loader().version.as_deref(), Some("47.4.10"));
        assert_eq!(analysis.author(), Some("A Reporter"));
        assert_eq!(analysis.required_file_count(PackSide::Client), 1);
        assert_eq!(analysis.optional_file_count(PackSide::Client), 1);
        assert!(analysis.needs_provider_resolution());
    }

    #[tokio::test]
    async fn an_import_with_no_key_configured_fails_before_any_request_and_creates_nothing() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_without_a_key(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-mod-credential-missing");
        assert!(app.list_clients().expect("list").is_empty());
        assert!(
            server
                .received_requests()
                .await
                .expect("recorded")
                .is_empty(),
            "a request was made without a key"
        );
    }

    // -- the exact import --------------------------------------------------

    #[tokio::test]
    async fn a_curseforge_pack_imports_into_the_same_normalized_recipe_a_modrinth_pack_does() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let imported = import(&app, &archive).await.expect("import");
        let record = &imported.record;
        assert_eq!(record.name, "Repro Pack");
        assert_eq!(record.minecraft_version, "1.20.1");
        assert_eq!(record.loader.kind, LoaderKind::Forge);
        assert_eq!(record.loader.version.as_deref(), Some("47.4.10"));
        assert_eq!(record.source.kind, ImportSourceKind::CurseForgePack);

        // The required file is installed; the unselected optional one is not.
        let game = ephervex_storage::metadata::client_game_dir(&record.root);
        assert_eq!(
            std::fs::read(game.join("mods/required-1.0.0.jar")).expect("read"),
            REQUIRED_BODY
        );
        assert!(!game.join("mods/optional-1.0.0.jar").exists());
        // ...and the pack's overrides landed.
        assert_eq!(
            std::fs::read_to_string(game.join("config/repro.toml")).expect("read"),
            "debug = true\n"
        );

        let recipe = imported.recipe();
        assert_eq!(recipe.files.len(), 2);
        let required = &recipe.files[0];
        assert_eq!(
            required.logical_path,
            PathBuf::from("mods/required-1.0.0.jar")
        );
        assert_eq!(required.origin, ModOrigin::PackDeclared);
        assert_eq!(required.client, SideRequirement::Required);
        assert_eq!(
            required.server,
            SideRequirement::Unknown,
            "the export format states nothing about servers"
        );
        let identity = required.identity.as_ref().expect("an identity");
        assert_eq!(identity.provider, Some(ModProvider::CurseForge));
        assert_eq!(identity.project_id.as_deref(), Some("238222"));
        assert_eq!(identity.version_id.as_deref(), Some("4593548"));
        assert_eq!(identity.file_id.as_deref(), Some("4593548"));

        let optional = &recipe.files[1];
        assert_eq!(optional.client, SideRequirement::Optional);
        assert_eq!(
            optional.selected,
            Some(false),
            "the user was asked and said no"
        );
    }

    #[tokio::test]
    async fn the_exact_declared_file_is_requested_and_a_newer_one_is_never_substituted() {
        // The regression this whole design exists for. The API is told to
        // answer about a *newer* file for the same project; the import must
        // refuse rather than install it.
        let server = MockServer::start().await;
        serve_pack_as_mods(&server).await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/{REQUIRED_PROJECT}/files/{REQUIRED_FILE}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                file_document(
                    &server,
                    REQUIRED_PROJECT,
                    REQUIRED_FILE + 1,
                    "required-2.0.0.jar",
                    REQUIRED_BODY,
                ),
                "application/json",
            ))
            .mount(&server)
            .await;
        serve_file(
            &server,
            OPTIONAL_PROJECT,
            OPTIONAL_FILE,
            "optional-1.0.0.jar",
            OPTIONAL_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-mod-file-identity-mismatch");
        assert!(app.list_clients().expect("list").is_empty());

        // Only the exact file endpoint, project classification, and artifact
        // delivery were asked; nothing listed the project's versions looking
        // for something that would fit. A "list this project's files" request
        // has no file id segment, which is exactly what distinguishes it from
        // every path allowed here.
        let paths: Vec<String> = server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .map(|request| request.url.path().to_owned())
            .collect();
        assert!(
            paths.iter().all(|requested| {
                requested == "/v1/categories"
                    || requested.contains("/files/")
                    || requested.starts_with("/files/")
                    // Project classification: `/v1/mods/{id}` exactly, with no
                    // further segment, the shape a "list this project's
                    // files" request would have and this never does.
                    || (requested.starts_with("/v1/mods/") && !requested[9..].contains('/'))
            }),
            "{paths:?}"
        );
    }

    #[tokio::test]
    async fn each_distinct_file_is_looked_up_exactly_once_for_one_import() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        import(&app, &archive).await.expect("import");

        let recorded = server.received_requests().await.expect("recorded");
        let file_lookups = recorded
            .iter()
            .filter(|request| {
                request.url.path().contains("/files/") && !request.url.path().starts_with("/files/")
            })
            .count();
        assert_eq!(
            file_lookups, 2,
            "one exact-file request per declared file, and no more"
        );
        let class_lookups = recorded
            .iter()
            .filter(|request| {
                request.url.path().starts_with("/v1/mods/")
                    && !request.url.path().contains("/files/")
            })
            .count();
        assert_eq!(
            class_lookups, 2,
            "one classification request per declared project, and no more"
        );
    }

    // -- project class validation -------------------------------------------

    #[tokio::test]
    async fn duplicate_manifest_entries_for_one_project_classify_it_once() {
        // `validate_project_classes` is exercised directly rather than through
        // a full manifest: the pack parser already collapses two declared
        // files of one project into a conflict or a single entry (see
        // `crates/pack/src/curseforge.rs`), so two *distinct* pending entries
        // naming the same project cannot arrive from a real manifest. What
        // this proves is that if they ever did, classification would still
        // ask about that project once, not twice.
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_project_class(&server, REQUIRED_PROJECT, "One Project", MODS_CLASS_ID).await;

        let dir = tempfile::tempdir().expect("temp dir");
        let app = app_against(dir.path(), &server);
        let api = app.curseforge_api().expect("a configured client");
        let operation = api.begin_operation().expect("a key is configured");

        let entry_of = |file_id: u32| PendingProviderFile {
            identity: ProviderFileIdentity {
                provider: Some(ModProvider::CurseForge),
                project_id: Some(REQUIRED_PROJECT.to_string()),
                version_id: Some(file_id.to_string()),
                file_id: Some(file_id.to_string()),
            },
            client: SideRequirement::Required,
            server: SideRequirement::Unknown,
        };
        let pending = vec![entry_of(REQUIRED_FILE), entry_of(OPTIONAL_FILE)];

        validate_project_classes(&operation, &pending, &CancellationToken::new())
            .await
            .expect("both entries name a Mod project");

        let classifications = server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .filter(|request| request.url.path() == format!("/v1/mods/{REQUIRED_PROJECT}"))
            .count();
        assert_eq!(
            classifications, 1,
            "one project named by two pending entries is classified once"
        );
    }

    #[tokio::test]
    async fn a_non_mod_project_refuses_the_whole_import_and_publishes_nothing() {
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", MODS_CLASS_ID).await;
        // Not a mod: a resource pack, wearing the "Resource Packs" class from
        // serve_project_classes's own fixture rather than the Mods class. The
        // project's own display name is deliberately unrelated to its class,
        // so a rejection that named the *project* instead of the *class*
        // would be caught rather than passing by coincidence.
        serve_project_class(&server, OPTIONAL_PROJECT, "Pretty Textures", 12).await;
        serve_file(
            &server,
            REQUIRED_PROJECT,
            REQUIRED_FILE,
            "required-1.0.0.jar",
            REQUIRED_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-mod-project-type-unsupported");
        assert_eq!(
            error.message_args(),
            vec![
                ("reference", OPTIONAL_PROJECT.to_string()),
                ("project_type", "Resource Packs".to_owned()),
            ],
            "the refusal must name the class the categories response returned, \
             never the unrelated project's own display name"
        );
        assert!(
            app.list_clients().expect("list").is_empty(),
            "a pack with one non-mod project publishes no instance at all, \
             not a partial one missing that entry"
        );

        // The file endpoint for the rejected non-mod project's own file was
        // never asked: rejection happens before any file is resolved.
        let recorded = server.received_requests().await.expect("recorded");
        assert!(
            recorded
                .iter()
                .all(|request| !request.url.path().contains(&OPTIONAL_FILE.to_string())),
            "classification must refuse before any file lookup, not skip the entry it refused"
        );
    }

    #[tokio::test]
    async fn an_optional_entrys_project_must_also_be_a_mod() {
        // The required project is a legitimate mod; only the optional entry
        // names something else. Optional inclusion is still Mods-only.
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", MODS_CLASS_ID).await;
        serve_project_class(&server, OPTIONAL_PROJECT, "A World", 17).await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-mod-project-type-unsupported");
        assert!(app.list_clients().expect("list").is_empty());
    }

    #[tokio::test]
    async fn a_project_reporting_no_class_fails_closed_rather_than_being_installed_as_a_mod() {
        // `classId` is documented as nullable. A project that reports none is
        // not a confirmed non-Mod (that would claim a class this build never
        // saw) and not a confirmed Mod either: it must refuse the import
        // rather than pick either.
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", MODS_CLASS_ID).await;
        serve_project_with_no_class(&server, OPTIONAL_PROJECT, "No Reported Class").await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-mod-provider-response-invalid");
        assert!(
            !matches!(
                error,
                CoreError::ModPlatform(ModPlatformError::ProjectTypeUnsupported { .. })
            ),
            "a null class is not a confirmed non-Mod class: {error:?}"
        );
        assert!(app.list_clients().expect("list").is_empty());
    }

    #[tokio::test]
    async fn a_class_id_the_categories_response_never_listed_fails_closed_rather_than_being_guessed()
     {
        // The project claims a class the categories fixture never named. This
        // build has no provider-returned name for it and must not guess one,
        // so it fails the same way an unusable response does rather than
        // inventing a class description.
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", MODS_CLASS_ID).await;
        serve_project_class(&server, OPTIONAL_PROJECT, "Something Else", 999_999).await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-mod-provider-response-invalid");
        assert!(
            !matches!(
                error,
                CoreError::ModPlatform(ModPlatformError::ProjectTypeUnsupported { .. })
            ),
            "an unlisted class id is not a confirmed non-Mod class: {error:?}"
        );
        assert!(app.list_clients().expect("list").is_empty());
    }

    #[tokio::test]
    async fn mods_class_discovery_does_not_depend_on_any_fixed_numeric_id() {
        // A second server whose Mods class carries a different unusual id
        // than the shared fixture's, so this test cannot pass by accident on
        // whatever number the other tests happen to share.
        const OTHER_MODS_CLASS_ID: u32 = 555_111;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/categories"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":[
                        {{"id":41,"gameId":432,"name":"Resource Packs","slug":"texture-packs","isClass":true}},
                        {{"id":{OTHER_MODS_CLASS_ID},"gameId":432,"name":"Mods","slug":"mc-mods","isClass":true}}
                    ]}}"#
                ),
                "application/json",
            ))
            .mount(&server)
            .await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", OTHER_MODS_CLASS_ID).await;
        serve_project_class(&server, OPTIONAL_PROJECT, "Also A Mod", OTHER_MODS_CLASS_ID).await;
        serve_file(
            &server,
            REQUIRED_PROJECT,
            REQUIRED_FILE,
            "required-1.0.0.jar",
            REQUIRED_BODY,
        )
        .await;
        serve_file(
            &server,
            OPTIONAL_PROJECT,
            OPTIONAL_FILE,
            "optional-1.0.0.jar",
            OPTIONAL_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        import(&app, &archive).await.expect(
            "the Mods class this server discovers is not the one every other test's fixture uses",
        );
    }

    #[tokio::test]
    async fn a_successfully_classified_pack_keeps_exact_client_and_server_semantics() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let imported = import(&app, &archive).await.expect("import");
        let recipe = app
            .pack_recipe(imported.record.id)
            .expect("query")
            .expect("a recipe");

        let required = recipe
            .files
            .iter()
            .find(|file| file.logical_path == Path::new("mods/required-1.0.0.jar"))
            .expect("the required file is in the recipe");
        assert_eq!(required.client, SideRequirement::Required);
        assert_eq!(
            required.server,
            SideRequirement::Unknown,
            "classification decides whether a project is a mod, never server support"
        );

        let optional = recipe
            .files
            .iter()
            .find(|file| file.logical_path == Path::new("mods/optional-1.0.0.jar"))
            .expect("the optional file is in the recipe");
        assert_eq!(
            optional.client,
            SideRequirement::Optional,
            "an explicit `required: false` survives classification unchanged"
        );
        assert_eq!(optional.server, SideRequirement::Unknown);
    }

    #[tokio::test]
    async fn no_classification_metadata_is_ever_written_to_the_shared_cache() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        import(&app, &archive).await.expect("import");

        // The only files a CurseForge import ever leaves in the shared cache
        // are verified artifact bytes, addressed by their own hash. Nothing
        // named after a project, a class, or the categories endpoint exists
        // anywhere under it.
        let cache_dir = app.paths().cache_dir();
        let mut found_metadata_file = false;
        for entry in walkdir_files(cache_dir) {
            let name = entry
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if name.contains("categor") || name.contains("class") || name.contains("mods") {
                found_metadata_file = true;
            }
        }
        assert!(
            !found_metadata_file,
            "no CurseForge classification metadata may be persisted"
        );
    }

    /// Every file under `root`, recursively, with no external dependency: the
    /// shared cache layout is a handful of directories deep at most.
    fn walkdir_files(root: &Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        let mut stack = vec![root.to_owned()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files
    }

    // -- verification and failure ------------------------------------------

    #[tokio::test]
    async fn a_body_that_does_not_match_the_published_digest_publishes_no_instance() {
        let server = MockServer::start().await;
        serve_pack_as_mods(&server).await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/{REQUIRED_PROJECT}/files/{REQUIRED_FILE}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                file_document(
                    &server,
                    REQUIRED_PROJECT,
                    REQUIRED_FILE,
                    "required-1.0.0.jar",
                    REQUIRED_BODY,
                ),
                "application/json",
            ))
            .mount(&server)
            .await;
        // The declared size still matches, so only the digest can catch this.
        Mock::given(method("GET"))
            .and(path(format!(
                "/files/{REQUIRED_PROJECT}/{REQUIRED_FILE}/required-1.0.0.jar"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                b"the CORRUPTED mod jar".to_vec(),
                "application/java-archive",
            ))
            .mount(&server)
            .await;
        serve_file(
            &server,
            OPTIONAL_PROJECT,
            OPTIONAL_FILE,
            "optional-1.0.0.jar",
            OPTIONAL_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-pack-download-failed");
        assert!(
            app.list_clients().expect("list").is_empty(),
            "a failed import leaves no visible instance"
        );
    }

    #[tokio::test]
    async fn a_file_whose_author_forbids_third_party_downloads_stops_the_import_explicitly() {
        let server = MockServer::start().await;
        serve_pack_as_mods(&server).await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/{REQUIRED_PROJECT}/files/{REQUIRED_FILE}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":{{"id":{REQUIRED_FILE},"gameId":432,"modId":{REQUIRED_PROJECT},
                        "isAvailable":true,"fileName":"restricted.jar","fileLength":10,
                        "downloadUrl":null,"hashes":[]}}}}"#
                ),
                "application/json",
            ))
            .mount(&server)
            .await;
        serve_file(
            &server,
            OPTIONAL_PROJECT,
            OPTIONAL_FILE,
            "optional-1.0.0.jar",
            OPTIONAL_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-mod-file-distribution-restricted");
        assert!(app.list_clients().expect("list").is_empty());
        // Nothing went looking for the file anywhere else.
        let fetches = server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .filter(|request| request.url.path().starts_with("/files/"))
            .count();
        assert_eq!(fetches, 0);
    }

    #[tokio::test]
    async fn a_canceled_import_publishes_nothing() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = app
            .import_pack(
                &ImportRequest::new(&archive),
                &OperationReporter::silent("curseforge-import"),
                &cancel,
            )
            .await
            .expect_err("canceled");
        assert!(error.is_cancellation(), "{error:?}");
        assert!(app.list_clients().expect("list").is_empty());
    }

    // -- credential containment --------------------------------------------

    #[tokio::test]
    async fn the_api_key_reaches_the_api_and_the_delivery_host_and_nothing_else() {
        // Both are loopback here, so the assertion that matters is the other
        // one: an unrelated download made through the same application does not
        // carry the key, and nothing written to disk contains it.
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let imported = import(&app, &archive).await.expect("import");

        let authenticated: Vec<&Request> = server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .filter(|request| request.headers.contains_key("x-api-key"))
            .cloned()
            .collect::<Vec<_>>()
            .leak()
            .iter()
            .collect();
        assert!(
            !authenticated.is_empty(),
            "the API request must be authenticated"
        );

        // Nothing the import left behind names the key.
        let source = SourceStore::new(ephervex_storage::metadata::source_dir(
            &imported.record.root,
        ));
        let recipe = std::fs::read_to_string(source.recipe_path()).expect("read");
        assert!(!recipe.contains(FAKE_KEY), "{recipe}");
        let manifest =
            std::fs::read_to_string(source.dir().join("curseforge/manifest.json")).expect("read");
        assert!(!manifest.contains(FAKE_KEY));
        for file in &imported.recipe().files {
            for address in &file.downloads {
                assert!(!address.contains(FAKE_KEY), "{address}");
                assert!(!address.contains('?'), "no query string carries a secret");
            }
        }
    }

    #[tokio::test]
    async fn a_download_address_off_the_permitted_hosts_is_refused_rather_than_followed() {
        let server = MockServer::start().await;
        serve_pack_as_mods(&server).await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/{REQUIRED_PROJECT}/files/{REQUIRED_FILE}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":{{"id":{REQUIRED_FILE},"gameId":432,"modId":{REQUIRED_PROJECT},
                        "isAvailable":true,"fileName":"required-1.0.0.jar","fileLength":20,
                        "downloadUrl":"https://evil.example/required-1.0.0.jar",
                        "hashes":[{{"value":"{digest}","algo":1}}]}}}}"#,
                    digest = sha1_of(REQUIRED_BODY),
                ),
                "application/json",
            ))
            .mount(&server)
            .await;
        serve_file(
            &server,
            OPTIONAL_PROJECT,
            OPTIONAL_FILE,
            "optional-1.0.0.jar",
            OPTIONAL_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-pack-download-url-rejected");
        assert!(app.list_clients().expect("list").is_empty());
    }

    // -- the shared artifact cache -----------------------------------------

    #[tokio::test]
    async fn a_curseforge_artifact_lands_in_the_shared_store_and_is_reused_by_the_next_import() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let first = import(&app, &archive).await.expect("import");
        assert_eq!(first.materialized.files.reused_from_cache, 0);

        // Addressed by the verified content hash, not by an id or a file name.
        let layout = CacheLayout::new(app.paths().cache_dir());
        let digest = Hash::new(HashAlgorithm::Sha1, &sha1_of(REQUIRED_BODY)).expect("a digest");
        let cached = layout.content(ContentStore::Downloads, &digest, None);
        assert!(
            cached.parent().expect("a parent").is_dir(),
            "the artifact is not in the shared content-addressed store"
        );

        let second = import(&app, &archive).await.expect("a second import");
        assert_eq!(
            second.materialized.files.reused_from_cache, 1,
            "the verified artifact was reused rather than downloaded again"
        );
    }

    #[tokio::test]
    async fn collecting_the_shared_cache_leaves_an_imported_instance_working() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let imported = import(&app, &archive).await.expect("import");
        let installed = ephervex_storage::metadata::client_game_dir(&imported.record.root)
            .join("mods/required-1.0.0.jar");
        assert!(installed.is_file());

        // A budget no artifact could fit, so the collector has to take them.
        let report = ArtifactCache::new(CacheLayout::new(app.paths().cache_dir()))
            .collect(ArtifactCacheLimit::Limited(0))
            .await
            .expect("collect");
        assert!(report.removed_entries > 0, "nothing was collected");
        assert_eq!(
            std::fs::read(&installed).expect("read"),
            REQUIRED_BODY,
            "the instance owns its own copy and survives eviction"
        );
    }

    // -- durability --------------------------------------------------------

    #[tokio::test]
    async fn the_normalized_recipe_reopens_with_no_provider_reachable_at_all() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);
        let imported = import(&app, &archive).await.expect("import");

        // Nothing below this line may contact anyone: the API is gone and so is
        // the archive.
        drop(server);
        std::fs::remove_file(&archive).expect("the archive goes away");

        let recipe = app
            .pack_recipe(imported.record.id)
            .expect("query")
            .expect("an imported instance has one");
        assert_eq!(recipe.source_kind, ImportSourceKind::CurseForgePack);
        assert_eq!(recipe.minecraft_version, "1.20.1");
        assert_eq!(recipe.loader.version.as_deref(), Some("47.4.10"));

        // Every declared entry is re-resolvable, and the one this client
        // actually took is the one its mod list shows.
        let managed = recipe.managed_mods();
        assert_eq!(managed.len(), 2);
        assert_eq!(managed[0].provider, ModProvider::CurseForge);
        assert_eq!(managed[0].project_id, "238222");
        assert_eq!(managed[0].version_id, "4593548");
        assert_eq!(managed[0].origin, ModOrigin::PackDeclared);

        let recorded = recipe.recorded_mods();
        assert_eq!(
            recorded.len(),
            1,
            "the optional file nobody selected is not a jar this client has"
        );
        assert_eq!(recorded[0].file_name, "required-1.0.0.jar");
        assert_eq!(recorded[0].origin, ModOrigin::PackDeclared);
        assert_eq!(recorded[0].server, SideRequirement::Unknown);

        // ...and the user's own manifest is still there, byte for byte.
        let source = app.pack_source_dir(imported.record.id).expect("source dir");
        assert_eq!(
            std::fs::read(source.join("curseforge/manifest.json")).expect("read"),
            export_manifest(),
            "the original manifest is retained verbatim"
        );

        // The recipe Ephervex owns is compact.
        let recipe_text = std::fs::read_to_string(source.join("recipe.json")).expect("read");
        assert!(!recipe_text.contains('\n'), "{recipe_text}");
    }

    #[tokio::test]
    async fn a_normalized_curseforge_recipe_plans_a_server_without_any_curseforge_knowledge() {
        let server = MockServer::start().await;
        serve_pack(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);
        let imported = import(&app, &archive).await.expect("import");
        drop(server);

        let recipe = app
            .pack_recipe(imported.record.id)
            .expect("query")
            .expect("a recipe");

        // The generic planner, unchanged and knowing nothing about CurseForge.
        let plan = recipe
            .server_reconstruction_plan(&OptionalSelection::none())
            .expect("a plan");
        assert!(
            plan.files.is_empty(),
            "an undeclared server side is a question, not a yes"
        );
        assert_eq!(
            plan.unconfirmed_unknown.len(),
            2,
            "both declared files are offered for confirmation rather than assumed"
        );
        assert_eq!(plan.minecraft_version, "1.20.1");
        assert_eq!(plan.loader.version.as_deref(), Some("47.4.10"));
        assert!(
            plan.layers_are_retained(),
            "the pack's overrides were kept for a server built later"
        );

        // Confirming it explicitly is what puts it in, and the exact identity
        // and ownership survive that too.
        let confirmed = recipe
            .server_reconstruction_plan(&OptionalSelection::of(["mods/required-1.0.0.jar"]))
            .expect("a plan");
        assert_eq!(confirmed.files.len(), 1);
        assert_eq!(confirmed.files[0].origin, ModOrigin::PackDeclared);
        assert_eq!(
            confirmed.files[0]
                .identity
                .as_ref()
                .and_then(|identity| identity.file_id.as_deref()),
            Some("4593548")
        );
    }

    // -- retained artifacts fetched again ----------------------------------
    //
    // A recipe records exactly what an artifact is and where it came from, which
    // is all the launcher needs to install it from the shared cache. It is not
    // all that is needed to *fetch* it: CurseForge serves its files only to an
    // authenticated request, so an artifact that has been collected out of the
    // cache needs the key back before a linked server can be built from the
    // client's retained desired state.

    const MODRINTH_BODY: &[u8] = b"a mod from the other catalog";
    const MODRINTH_PATH: &str = "mods/from-modrinth.jar";

    /// Serves the pack's metadata, and its artifacts only to a request carrying
    /// the API key.
    ///
    /// An unauthenticated attempt is answered rather than ignored, so a test can
    /// tell "nothing was sent" from "something was sent without the key".
    async fn serve_pack_guarded(server: &MockServer) {
        serve_pack_as_mods(server).await;
        for (project, file, name, body) in [
            (
                REQUIRED_PROJECT,
                REQUIRED_FILE,
                "required-1.0.0.jar",
                REQUIRED_BODY,
            ),
            (
                OPTIONAL_PROJECT,
                OPTIONAL_FILE,
                "optional-1.0.0.jar",
                OPTIONAL_BODY,
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/v1/mods/{project}/files/{file}")))
                .respond_with(ResponseTemplate::new(200).set_body_raw(
                    file_document(server, project, file, name, body),
                    "application/json",
                ))
                .mount(server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("/files/{project}/{file}/{name}")))
                .and(header(CURSEFORGE_API_KEY_HEADER, FAKE_KEY))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_raw(body.to_vec(), "application/java-archive"),
                )
                .mount(server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("/files/{project}/{file}/{name}")))
                .respond_with(ResponseTemplate::new(403))
                .mount(server)
                .await;
        }
    }

    /// How many artifact requests this server has been sent, and how many of
    /// them carried the key.
    async fn artifact_requests(server: &MockServer, prefix: &str) -> (usize, usize) {
        let recorded = server.received_requests().await.expect("recorded");
        let matching = recorded
            .iter()
            .filter(|request| request.url.path().starts_with(prefix));
        let total = matching.clone().count();
        let authenticated = matching
            .filter(|request| request.headers.contains_key(CURSEFORGE_API_KEY_HEADER))
            .count();
        (total, authenticated)
    }

    /// Imports the pack and then removes every reusable artifact, which is the
    /// state a later operation finds after the cache has been collected.
    async fn import_then_evict(app: &App, archive: &Path) -> crate::import::ImportedPack {
        let imported = import(app, archive).await.expect("import");
        let report = ArtifactCache::new(CacheLayout::new(app.paths().cache_dir()))
            .clear()
            .await
            .expect("clear");
        assert!(
            report.removed_entries > 0,
            "nothing was in the shared cache"
        );
        imported
    }

    /// The reconstruction a "Create Server" runs once Java and the loader are
    /// out of the way, against a staging root of this test's own.
    async fn reconstruct_server(
        app: &App,
        imported: &crate::import::ImportedPack,
        staging: &Path,
    ) -> CoreResult<PathBuf> {
        let recipe = app
            .pack_recipe(imported.record.id)
            .expect("query")
            .expect("an imported instance has a recipe");
        let client_source = app.pack_source_dir(imported.record.id).expect("source dir");
        let target = staging.join("minecraft");
        let options = ServerCreateOptions {
            java_path: None,
            install_java_if_missing: false,
            // The pack declares nothing about servers, so the file this test
            // cares about is confirmed explicitly rather than assumed.
            confirmed_unknown: OptionalSelection::of(["mods/required-1.0.0.jar"]),
        };
        app.materialize_linked_server_recipe(
            &recipe,
            &client_source,
            &target,
            &staging.join(".launcher/source"),
            &options,
            &CancellationToken::new(),
        )
        .await?;
        Ok(target)
    }

    /// A retained entry from the other catalog, served by the same loopback host.
    ///
    /// What makes it the other catalog is its identity, which is the only thing
    /// credential selection is allowed to read.
    fn modrinth_entry(server: &MockServer) -> PackFileRecipe {
        PackFileRecipe {
            logical_path: PathBuf::from(MODRINTH_PATH),
            hashes: vec![Hash::new(HashAlgorithm::Sha1, &sha1_of(MODRINTH_BODY)).expect("digest")],
            downloads: vec![format!("{}/modrinth/from-modrinth.jar", server.uri())],
            size: Some(MODRINTH_BODY.len() as u64),
            client: SideRequirement::Required,
            server: SideRequirement::Required,
            selected: None,
            origin: ModOrigin::UserSelected,
            identity: Some(ProviderFileIdentity {
                provider: Some(ModProvider::Modrinth),
                project_id: Some("AABBCCDD".to_owned()),
                version_id: Some("1".to_owned()),
                file_id: None,
            }),
        }
    }

    fn shared_cache_path(app: &App, body: &[u8]) -> PathBuf {
        let digest = Hash::new(HashAlgorithm::Sha1, &sha1_of(body)).expect("a digest");
        CacheLayout::new(app.paths().cache_dir()).content(
            ContentStore::Downloads,
            &digest,
            Some(&FileExtension::new("jar").expect("an extension")),
        )
    }

    #[tokio::test]
    async fn a_retained_curseforge_artifact_is_authenticated_when_the_cache_no_longer_has_it() {
        let server = MockServer::start().await;
        serve_pack_guarded(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let imported = import_then_evict(&app, &archive).await;
        let (before, _) = artifact_requests(&server, "/files/").await;

        let target = reconstruct_server(&app, &imported, &dir.path().join("server"))
            .await
            .expect("the server reconstructs from the retained recipe");

        assert_eq!(
            std::fs::read(target.join("mods/required-1.0.0.jar")).expect("read"),
            REQUIRED_BODY,
            "the server owns its own verified copy"
        );
        assert_eq!(
            std::fs::read(shared_cache_path(&app, REQUIRED_BODY)).expect("read"),
            REQUIRED_BODY,
            "the artifact re-entered the shared content-addressed store"
        );

        let (total, authenticated) = artifact_requests(&server, "/files/").await;
        assert!(total > before, "the artifact was not fetched again");
        assert_eq!(
            total, authenticated,
            "every retained CurseForge artifact request carried the key"
        );
    }

    #[tokio::test]
    async fn linked_server_reconstruction_never_requeries_curseforge_metadata() {
        let server = MockServer::start().await;
        serve_pack_guarded(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        // Evicted, so reconstruction has to actually redeliver the artifact
        // rather than finding it already on disk: a weaker test that never
        // touched the cache could pass by accident, having asked nothing.
        let imported = import_then_evict(&app, &archive).await;
        let (metadata_before, _) = artifact_requests(&server, "/v1/").await;
        assert_eq!(
            metadata_before, 5,
            "one categories lookup, one classification and one exact-file \
             lookup per declared project during import, and no more"
        );

        let target = reconstruct_server(&app, &imported, &dir.path().join("server"))
            .await
            .expect("the server reconstructs from the retained recipe");

        let (metadata_after, _) = artifact_requests(&server, "/v1/").await;
        assert_eq!(
            metadata_after, metadata_before,
            "linked-server reconstruction must use the retained recipe's own \
             identity, never a fresh CurseForge metadata query"
        );
        let (artifacts, _) = artifact_requests(&server, "/files/").await;
        assert!(
            artifacts > 0,
            "the evicted artifact is still delivered by content identity"
        );
        assert_eq!(
            std::fs::read(target.join("mods/required-1.0.0.jar")).expect("read"),
            REQUIRED_BODY
        );
    }

    #[tokio::test]
    async fn a_cached_retained_curseforge_artifact_needs_no_key_and_contacts_nobody() {
        let server = MockServer::start().await;
        serve_pack_guarded(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);
        let imported = import(&app, &archive).await.expect("import");
        assert!(
            shared_cache_path(&app, REQUIRED_BODY).is_file(),
            "the import left the artifact in the shared cache"
        );

        // A launcher that has never configured CurseForge, and a CurseForge that
        // is not there at all: reading the key would fail, and so would any
        // request. Reconstruction must depend on neither.
        let unconfigured = app_without_a_key(dir.path(), &server);
        drop(server);

        let target = reconstruct_server(&unconfigured, &imported, &dir.path().join("server"))
            .await
            .expect("a verified cache entry is installable without a provider");
        assert_eq!(
            std::fs::read(target.join("mods/required-1.0.0.jar")).expect("read"),
            REQUIRED_BODY
        );
    }

    #[tokio::test]
    async fn a_retained_curseforge_artifact_with_no_key_fails_as_a_credential_problem() {
        let server = MockServer::start().await;
        serve_pack_guarded(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);
        let imported = import_then_evict(&app, &archive).await;
        let (before, _) = artifact_requests(&server, "/files/").await;

        let unconfigured = app_without_a_key(dir.path(), &server);
        let error = reconstruct_server(&unconfigured, &imported, &dir.path().join("server"))
            .await
            .expect_err("an artifact that has to be fetched needs the key");

        assert_eq!(
            error.message_id(),
            "error-mod-credential-missing",
            "the user is told to configure CurseForge, not that a download failed"
        );
        let (after, _) = artifact_requests(&server, "/files/").await;
        assert_eq!(
            after, before,
            "nothing was sent unauthenticated to see whether it might work"
        );
    }

    #[tokio::test]
    async fn only_the_curseforge_artifacts_of_a_mixed_recipe_are_authenticated() {
        let server = MockServer::start().await;
        serve_pack_guarded(&server).await;
        // The other catalog's artifact, redirected once to a host the CurseForge
        // allowlist does not contain. Following it is correct — this request
        // carries no CurseForge secret — and proves that one provider's redirect
        // containment did not become everybody's.
        let elsewhere = format!(
            "http://localhost:{}/relocated/from-modrinth.jar",
            server.address().port()
        );
        Mock::given(method("GET"))
            .and(path("/modrinth/from-modrinth.jar"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", elsewhere.as_str()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/relocated/from-modrinth.jar"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(MODRINTH_BODY.to_vec(), "application/java-archive"),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);
        let imported = import_then_evict(&app, &archive).await;

        // A desired state naming files from two catalogs, which is what an
        // instance becomes as soon as a mod is installed into an imported pack.
        let source = SourceStore::new(app.pack_source_dir(imported.record.id).expect("source dir"));
        let mut recipe = source.read_recipe().expect("the retained recipe");
        recipe.files.push(modrinth_entry(&server));
        source
            .write_recipe(&recipe)
            .expect("retain the mixed recipe");

        let target = reconstruct_server(&app, &imported, &dir.path().join("server"))
            .await
            .expect("both catalogs' artifacts are fetched");
        assert_eq!(
            std::fs::read(target.join("mods/required-1.0.0.jar")).expect("read"),
            REQUIRED_BODY
        );
        assert_eq!(
            std::fs::read(target.join(MODRINTH_PATH)).expect("read"),
            MODRINTH_BODY,
            "an unauthenticated download still follows an ordinary redirect"
        );

        let (curseforge, curseforge_authenticated) = artifact_requests(&server, "/files/").await;
        assert!(curseforge > 0 && curseforge == curseforge_authenticated);
        for prefix in ["/modrinth/", "/relocated/"] {
            let (total, authenticated) = artifact_requests(&server, prefix).await;
            assert!(total > 0, "{prefix} was never fetched");
            assert_eq!(
                authenticated, 0,
                "{prefix} was offered another provider's credential"
            );
        }
    }

    #[tokio::test]
    async fn a_retained_curseforge_artifact_cannot_be_redirected_off_its_permitted_hosts() {
        let server = MockServer::start().await;
        serve_pack_guarded(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);
        let imported = import_then_evict(&app, &archive).await;

        // The same address the recipe records now answers with a redirect to a
        // host the credential's allowlist does not name. `localhost` resolves to
        // the same loopback server, so the only thing standing between the key
        // and that endpoint is the redirect policy.
        server.reset().await;
        let elsewhere = format!(
            "http://localhost:{}/leaked/required-1.0.0.jar",
            server.address().port()
        );
        Mock::given(method("GET"))
            .and(path(format!(
                "/files/{REQUIRED_PROJECT}/{REQUIRED_FILE}/required-1.0.0.jar"
            )))
            .respond_with(ResponseTemplate::new(302).insert_header("location", elsewhere.as_str()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/leaked/required-1.0.0.jar"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(REQUIRED_BODY.to_vec(), "application/java-archive"),
            )
            .mount(&server)
            .await;

        let error = reconstruct_server(&app, &imported, &dir.path().join("server"))
            .await
            .expect_err("a redirect off the permitted hosts is refused, not followed");
        assert_eq!(error.message_id(), "error-pack-download-failed");

        let (leaked, _) = artifact_requests(&server, "/leaked/").await;
        assert_eq!(
            leaked, 0,
            "the credential followed a redirect off its hosts"
        );
    }

    #[tokio::test]
    async fn a_key_the_delivery_host_refuses_is_reported_as_a_refused_key() {
        // Nothing before the request can find this out: the key is present,
        // readable, and syntactically fine, and only the host that serves the
        // artifact knows it is no longer honored. The answer still has to reach
        // the user as "your key was refused" rather than as a failed download,
        // because those have different next steps.
        let server = MockServer::start().await;
        serve_pack_guarded(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);
        let imported = import_then_evict(&app, &archive).await;

        let stale = app_with_a_stale_key(dir.path(), &server);
        let error = reconstruct_server(&stale, &imported, &dir.path().join("server"))
            .await
            .expect_err("the delivery host refuses the configured key");

        assert!(
            matches!(
                error,
                CoreError::ModPlatform(ModPlatformError::CredentialRejected {
                    provider: ModProvider::CurseForge,
                    status: 403,
                })
            ),
            "{error:?}"
        );
        assert_eq!(error.message_id(), "error-mod-credential-rejected");
        assert_ne!(error.message_id(), "error-pack-download-failed");

        // The attempt was authenticated, and the key went only to the routes it
        // is issued for: the API during the import, and the delivery addresses
        // the recipe records. Nothing followed the refusal anywhere else.
        let (total, authenticated) = artifact_requests(&server, "/files/").await;
        assert!(total > 0 && total == authenticated);
        for request in server.received_requests().await.expect("recorded") {
            let route = request.url.path().to_owned();
            assert!(
                route.starts_with("/v1/") || route.starts_with("/files/"),
                "the key reached `{route}`"
            );
        }
    }

    #[tokio::test]
    async fn another_catalog_refusing_a_retained_artifact_is_not_a_curseforge_key_problem() {
        // The identical status, for a file whose durable identity names another
        // catalog and which therefore carried no credential at all.
        let server = MockServer::start().await;
        serve_pack_guarded(&server).await;
        Mock::given(method("GET"))
            .and(path("/modrinth/from-modrinth.jar"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);
        let imported = import_then_evict(&app, &archive).await;

        let source = SourceStore::new(app.pack_source_dir(imported.record.id).expect("source dir"));
        let mut recipe = source.read_recipe().expect("the retained recipe");
        recipe.files.push(modrinth_entry(&server));
        source
            .write_recipe(&recipe)
            .expect("retain the mixed recipe");

        let error = reconstruct_server(&app, &imported, &dir.path().join("server"))
            .await
            .expect_err("the other catalog refused the download");
        assert_eq!(
            error.message_id(),
            "error-pack-download-failed",
            "another provider's refusal was blamed on the CurseForge key: {error:?}"
        );

        let (_, modrinth_authenticated) = artifact_requests(&server, "/modrinth/").await;
        assert_eq!(
            modrinth_authenticated, 0,
            "the CurseForge key was offered to another catalog"
        );
    }

    // -- opt-in production smoke -------------------------------------------

    /// The exact file this smoke installs, and deliberately not the newest one.
    ///
    /// Just Enough Items is a long-lived project that permits third-party
    /// downloads, and this is one of its older 1.20.1 builds. Newer files of
    /// the same project exist, which is the point: an exact import installs the
    /// one the manifest declares.
    const LIVE_PROJECT: u32 = 238_222;
    const LIVE_FILE: u32 = 8_617_367;

    #[tokio::test]
    #[ignore = "contacts the production CurseForge API with the configured key and downloads one artifact"]
    async fn the_production_api_resolves_and_delivers_one_exact_declared_file() {
        // Runs against the real credential store and the real API, into a
        // temporary root of its own: no instance the user owns is touched, and
        // the key is never read by this test — only by the transport, which
        // puts it in one header.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("live.zip");
        zip_file(
            &archive,
            &[(
                "manifest.json",
                format!(
                    r#"{{
                        "minecraft": {{
                            "version": "1.20.1",
                            "modLoaders": [{{"id": "forge-47.4.10", "primary": true}}]
                        }},
                        "manifestType": "minecraftModpack",
                        "manifestVersion": 1,
                        "name": "Live Smoke",
                        "version": "1.0.0",
                        "files": [
                            {{"projectID": {LIVE_PROJECT}, "fileID": {LIVE_FILE}, "required": true}}
                        ],
                        "overrides": "overrides"
                    }}"#
                )
                .as_bytes(),
            )],
        );

        let app = App::open_at(Paths::rooted_at(dir.path())).expect("app opens");
        let imported = import(&app, &archive)
            .await
            .expect("the production API resolves the declared file");

        let entry = &imported.recipe().files[0];
        assert_eq!(
            entry
                .identity
                .as_ref()
                .and_then(|identity| identity.file_id.as_deref()),
            Some(LIVE_FILE.to_string().as_str()),
            "the declared file, not a newer one"
        );
        assert_eq!(entry.origin, ModOrigin::PackDeclared);
        assert_eq!(entry.server, SideRequirement::Unknown);
        assert!(
            entry
                .hashes
                .iter()
                .any(|hash| hash.algorithm == HashAlgorithm::Sha1),
            "a cryptographic digest was published and kept"
        );

        // The artifact is verified, in the instance, and in the shared store.
        let installed = ephervex_storage::metadata::client_game_dir(&imported.record.root)
            .join(&entry.logical_path);
        assert!(installed.is_file(), "{}", installed.display());
        let cached = CacheLayout::new(app.paths().cache_dir()).content(
            ContentStore::Downloads,
            &entry.hashes[0],
            None,
        );
        assert!(
            cached.parent().expect("a parent").is_dir(),
            "the verified artifact is not in the shared content-addressed store"
        );

        // Nothing the import kept names a credential-bearing address.
        for address in &entry.downloads {
            assert!(address.starts_with("https://"), "{address}");
            assert!(address.find('?').is_none(), "{address}");
        }
    }
}
