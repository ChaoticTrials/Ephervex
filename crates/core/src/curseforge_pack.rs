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
//! resolve it, and gets back an analysis it can display and import - the same
//! two steps a Modrinth pack takes, where the second happens to have nothing to
//! do.
//!
//! # What is exact about it
//!
//! Every lookup is `project P, file F` against the endpoint that answers about
//! exactly that file, and the answer is checked against the question before it
//! is used. Nothing here lists a project's versions, and nothing picks a newer
//! file when the declared one is gone: a missing exact file fails the import.
//! The one file this module *does* choose between is two files the manifest
//! itself declared for the same project - see [`resolve_duplicate_projects`]
//! and ADR 0031 - which is a defect in the pack's own declaration, not a
//! missing file this module went looking for a substitute for.
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
//! rebuilt from a recipe asks the API what an instance contains - but the bytes
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
//! model needs to install and verify an artifact - a name, a size, a digest,
//! and the address it came from. The response itself is parsed, used, and
//! dropped. See ADR 0022.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use ephervex_download::{
    AuthenticatedSource, DomainAllowlist, DownloadError, DownloadManager, DownloaderConfig,
    HostScope,
};
use ephervex_mod_platform::{
    CURSEFORGE_API_KEY_HEADER, CurseForgeFileRelease, CurseForgeOperation, CurseForgeProject,
    CurseForgeProjectClass, MINECRAFT_GAME_ID, ModPlatformError, file_browser_url,
};
use ephervex_model::{
    AttentionReason, CurseForgeFileId, CurseForgeProjectId, Hash, ModOrigin, ModProvider,
    SecretString,
};
use ephervex_pack::{
    ArtifactCredentials, BlockedProviderFile, CredentialUnavailable, DuplicateFileReason,
    DuplicateProviderFile, MODS_DIR, PackAnalysis, PackError, PackFileRecipe, PackFormat,
    PendingProviderFile, ProviderFileIdentity, RESOURCEPACKS_DIR, RecipeRelation, SHADERPACKS_DIR,
    safe_file_name,
};
use futures_util::StreamExt as _;
use reqwest::header::HeaderName;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument};

use crate::app::App;
use crate::error::{CoreError, CoreResult};

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
        skip_blocked: &[ProviderFileIdentity],
        cancel: &CancellationToken,
    ) -> CoreResult<()> {
        self.prepare_provider_files(analysis, skip_blocked, cancel)
            .await?;
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
        skip_blocked: &[ProviderFileIdentity],
        cancel: &CancellationToken,
    ) -> CoreResult<Option<Arc<AuthenticatedSource>>> {
        match analysis.format {
            PackFormat::CurseForgePack => Ok(Some(
                self.resolve_curseforge_pack(analysis, skip_blocked, cancel)
                    .await?,
            )),
            // Every other format's manifest describes its own files, so there
            // is nothing to resolve and no credential to spend.
            PackFormat::ModrinthPack | PackFormat::PrismExport | _ => Ok(None),
        }
    }

    #[instrument(skip_all, fields(pending = analysis.pending.len()))]
    async fn resolve_curseforge_pack(
        &self,
        analysis: &mut PackAnalysis,
        skip_blocked: &[ProviderFileIdentity],
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
        let classes = classify_projects(&operation, &analysis.pending, cancel).await?;

        let pending = std::mem::take(&mut analysis.pending);
        let (mut resolved, blocked, duplicates) = match resolve_files(
            &operation,
            &pending,
            &classes,
            &hosts,
            &analysis.recipe.minecraft_version,
            cancel,
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                // The analysis is left describing the pack it describes: a
                // caller that retries after configuring a key must still see
                // the entries that need resolving.
                analysis.pending = pending;
                return Err(error);
            }
        };
        analysis.duplicate_files.extend(duplicates);

        // A file the provider refused is not necessarily still missing: an
        // earlier import, or a folder a watcher is minding, may have already
        // put a verified copy in the shared cache. Only a cache miss the
        // caller explicitly chose to skip becomes an attention reason; every
        // other one stays blocked for display.
        let mut still_blocked = Vec::with_capacity(blocked.len());
        for entry in blocked {
            let content_class = entry
                .identity
                .project_id
                .as_deref()
                .and_then(CurseForgeProjectId::parse)
                .and_then(|project| classes.get(&project))
                .map(|classified| classified.content);
            let cached = match content_class {
                Some(content_class) => self.recipe_from_cache(&entry, content_class).await?,
                None => None,
            };
            match cached {
                Some(recipe) => resolved.push(recipe),
                None if skip_blocked.contains(&entry.identity) => {
                    analysis
                        .attention
                        .push(AttentionReason::ManualDownloadRequired(
                            manual_download_detail(&entry),
                        ));
                }
                None => still_blocked.push(entry),
            }
        }

        info!(
            files = resolved.len(),
            blocked = still_blocked.len(),
            "resolved a CurseForge pack's declared files"
        );
        analysis.recipe.files.extend(resolved);
        analysis.blocked = still_blocked;
        Ok(credential)
    }

    /// A blocked file's recipe entry, if the shared cache already holds bytes
    /// matching its digest.
    ///
    /// Checked with no network access: a verified hash at the content-addressed
    /// path is exactly the proof an ordinary completed download leaves behind,
    /// so a file a folder watcher (or an earlier import) already placed there
    /// needs no download address at all. `None` for a file with no usable name
    /// or no published hash, not just a genuine cache miss - either one means
    /// there is nothing this launcher could have verified a hand-placed copy
    /// against.
    async fn recipe_from_cache(
        &self,
        blocked: &BlockedProviderFile,
        content_class: PackContentClass,
    ) -> CoreResult<Option<PackFileRecipe>> {
        let Some(name) = blocked.file_name.as_deref().and_then(safe_file_name) else {
            return Ok(None);
        };
        let Some(bytes) = self
            .cached_artifact_bytes(&blocked.hashes, blocked.file_name.as_deref())
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(PackFileRecipe {
            logical_path: PathBuf::from(content_class.logical_dir()).join(&name),
            hashes: blocked.hashes.clone(),
            downloads: Vec::new(),
            size: Some(bytes),
            client: blocked.client,
            server: blocked.server,
            selected: None,
            origin: ModOrigin::PackDeclared,
            identity: Some(blocked.identity.clone()),
            original_identity: None,
            // The provider refused to serve this file's bytes, not to describe
            // it. Whatever its document declared is recorded here; `None`
            // survives only where nothing ever described the file at all.
            relations: blocked.relations.clone(),
        }))
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

    /// The hosts a Modrinth file, or a pack file whose identity names no
    /// catalog, may be fetched from. A method for the same reason as
    /// [`Self::curseforge_artifact_hosts`].
    #[cfg_attr(
        not(test),
        expect(
            clippy::unused_self,
            reason = "only a test build reads the loopback flag"
        )
    )]
    pub(crate) fn modrinth_artifact_hosts(&self) -> DomainAllowlist {
        // `localhost` stands in for the release-asset hosts a listed address
        // may redirect to.
        #[cfg(test)]
        if self.loopback_http {
            return DomainAllowlist::for_loopback_tests("127.0.0.1")
                .allowing_redirects_to("localhost");
        }
        DomainAllowlist::modrinth_pack_defaults()
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
            curseforge_hosts: OnceLock::new(),
            modrinth_hosts: OnceLock::new(),
        }
    }
}

/// Supplies the provider credential a single retained artifact needs.
///
/// # Why retained desired state can still need one
///
/// A recipe entry records the exact addresses, digests, and size of its
/// artifact, and that is all the launcher needs to *identify and verify* it -
/// forever, offline, and without asking any provider anything. What it does not
/// change is who serves the bytes. One provider serves its artifacts only to a
/// request carrying an API key, so re-fetching such a file after its verified
/// copy has been collected out of the shared artifact cache needs that key,
/// even though nothing about the file itself is in question.
///
/// The consequences are worth stating in both directions:
///
/// * a verified artifact still in the shared cache is installed without any
///   credential, any provider request, or any configuration at all - which is
///   what keeps an installed instance working on a machine that has configured
///   nothing;
/// * an artifact that has to come back over the network is subject to whatever
///   its own provider requires.
///
/// # One credential per provider, chosen per file
///
/// The decision is made from [`ProviderFileIdentity::provider`], the durable
/// fact the recipe records, and from nothing else - never from an address, a
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
    curseforge_hosts: OnceLock<Arc<HostScope>>,
    modrinth_hosts: OnceLock<Arc<HostScope>>,
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
        // `ModPlatformError::CredentialMissing` when nothing is configured -
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

    fn hosts_for(
        &self,
        identity: Option<&ProviderFileIdentity>,
    ) -> Result<Option<Arc<HostScope>>, DownloadError> {
        let (cell, hosts) = match identity.and_then(|identity| identity.provider) {
            Some(ModProvider::CurseForge) => {
                (&self.curseforge_hosts, self.app.curseforge_artifact_hosts())
            }
            // A pack index names no catalog; its addresses were held to
            // Modrinth's pack host list when it was read.
            Some(ModProvider::Modrinth) | None => {
                (&self.modrinth_hosts, self.app.modrinth_artifact_hosts())
            }
            // A catalog with no list of its own vouches for no host.
            Some(_) => {
                return Ok(Some(Arc::new(HostScope::new(
                    DomainAllowlist::empty(),
                    &self.app.http_config(),
                )?)));
            }
        };
        if let Some(scope) = cell.get() {
            return Ok(Some(Arc::clone(scope)));
        }
        let scope = Arc::new(HostScope::new(hosts, &self.app.http_config())?);
        Ok(Some(Arc::clone(cell.get_or_init(|| scope))))
    }
}

/// Reports a credential this launcher could not assemble as a provider
/// credential failure rather than as whatever it was underneath.
///
/// The user-facing outcome is the same either way - CurseForge cannot be used
/// until something is fixed - and keeping one shape means a frontend has one
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

/// Where a CurseForge pack-import project's exact file lands, once its class
/// has been confirmed against this operation's own category response.
///
/// Narrower than [`CurseForgeProjectClass`] on purpose: by the time a
/// [`PackFileRecipe`] is built, a project this build does not import has
/// already turned into a refusal in [`classify_projects`], so
/// [`recipe_entry`] only ever has to route between the classes the pack
/// importer actually accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackContentClass {
    Mods,
    ResourcePacks,
    ShaderPacks,
}

impl PackContentClass {
    /// The directory this class installs into, relative to the game
    /// directory.
    const fn logical_dir(self) -> &'static str {
        match self {
            Self::Mods => MODS_DIR,
            Self::ResourcePacks => RESOURCEPACKS_DIR,
            Self::ShaderPacks => SHADERPACKS_DIR,
        }
    }
}

/// What [`classify_projects`] learned about one declared project: enough to
/// route its files, and enough to name and link the project if one of them
/// turns out to be blocked.
#[derive(Debug, Clone)]
struct ClassifiedProject {
    content: PackContentClass,
    name: String,
    slug: String,
    /// The class-page path segment CurseForge's own site uses, from the same
    /// classification `content` came from. Always present for a project this
    /// function keeps (`Other` is refused before a [`ClassifiedProject`] is
    /// built), but carried as an `Option` rather than asserted, so a future
    /// provider change that widens the accepted classes cannot silently
    /// produce a broken link.
    browser_path_segment: Option<&'static str>,
}

/// Confirms every project a manifest declares against CurseForge's own
/// category classes, before any file is fetched, and says where each one's
/// file belongs.
///
/// A CurseForge manifest names its files by project id alone, and nothing
/// stops a project id from naming a Resource Pack, a World, or any other
/// class of content. Unlike the standalone mod browser, a modpack import
/// legitimately contains more than Mods: a confirmed Mod routes to `mods/`,
/// a confirmed Resource Pack routes to `resourcepacks/`, and a confirmed
/// Shader Pack routes to `shaderpacks/`, all by the one authoritative
/// classification [`ephervex_mod_platform::CurseForgeProjectClasses::classify`]
/// provides. Every other known class is a pack the whole import refuses
/// rather than silently omits, and a class this build cannot confirm at all
/// (a missing `classId`, or one the categories response never listed) fails
/// the same way rather than being guessed at from a file name or a title. A
/// pack missing one file it declared, or carrying one this build cannot
/// place, is not the pack its manifest describes.
///
/// Deduplicated by project, the same as [`resolve_files`] deduplicates by
/// file: a pack that declares one project's optional and required files
/// together asks about that project once.
async fn classify_projects(
    operation: &CurseForgeOperation<'_>,
    pending: &[PendingProviderFile],
    cancel: &CancellationToken,
) -> CoreResult<BTreeMap<CurseForgeProjectId, ClassifiedProject>> {
    let mut projects: Vec<CurseForgeProjectId> = pending
        .iter()
        .map(|entry| declared_ids(entry).map(|(project, _)| project))
        .collect::<CoreResult<_>>()?;
    projects.sort_unstable();
    projects.dedup();

    // Discovered fresh for this operation. Which numeric id means "Mods" and
    // which means "Resource Packs" is provider metadata, not something this
    // launcher is allowed to assume.
    let classes = operation.project_classes(MINECRAFT_GAME_ID, cancel).await?;

    // `buffered` yields responses in the same order the requests were made,
    // so this stays lined up with `projects` without re-deriving an identity
    // from the response.
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

    projects
        .into_iter()
        .zip(classified)
        .map(|(id, project)| {
            let classification = classes.classify(&project)?;
            let browser_path_segment = classification.browser_path_segment();
            let content = match classification {
                CurseForgeProjectClass::Mods => PackContentClass::Mods,
                CurseForgeProjectClass::ResourcePacks => PackContentClass::ResourcePacks,
                CurseForgeProjectClass::ShaderPacks => PackContentClass::ShaderPacks,
                // A real, known CurseForge class this build still does not
                // import. Named in pack-import terms: a modpack legitimately
                // contains more than mods, so this is not the mod browser's
                // "not a mod" refusal.
                CurseForgeProjectClass::Other(project_type) => {
                    return Err(CoreError::from(PackError::ProviderContentUnsupported {
                        provider: ModProvider::CurseForge,
                        reference: project.id.to_string(),
                        project_type,
                    }));
                }
            };
            Ok((
                id,
                ClassifiedProject {
                    content,
                    name: project.name,
                    slug: project.slug,
                    browser_path_segment,
                },
            ))
        })
        .collect()
}

/// What one distribution-restricted file's response said, before it is
/// attached to each pending entry that declared it.
struct BlockedFileDetail {
    project_name: Option<String>,
    file_name: Option<String>,
    browser_url: Option<String>,
    hashes: Vec<Hash>,
    /// The relations the same response declared. A file the provider will not
    /// serve is still a file it described, so this is knowledge rather than a
    /// gap (ADR 0034).
    relations: Vec<RecipeRelation>,
}

/// Looks up every declared file, deduplicated and with bounded concurrency.
///
/// A file whose author disabled third-party downloads does not abort the
/// batch: the provider still describes it, and it is collected into the
/// second return value instead, one entry per pending file it applies to, so
/// a caller can display it, offer a manual-download link, or resolve it from
/// the shared cache rather than losing the rest of the pack over one author's
/// decision. Everything that response declared travels with it, relations
/// included. Every lookup failure still propagates immediately and aborts the
/// batch, exactly as before.
///
/// A project two `pending` entries both declare (ADR 0031) is resolved down
/// to one winner - see [`pick_winner`] - before recipe entries are built; the
/// third return value reports which file was kept over which, by name, for
/// every such project.
///
/// Returns resolved entries in the pack's own order, so the recipe reads the
/// way the manifest does.
async fn resolve_files(
    operation: &CurseForgeOperation<'_>,
    pending: &[PendingProviderFile],
    classes: &BTreeMap<CurseForgeProjectId, ClassifiedProject>,
    hosts: &DomainAllowlist,
    minecraft_version: &str,
    cancel: &CancellationToken,
) -> CoreResult<(
    Vec<PackFileRecipe>,
    Vec<BlockedProviderFile>,
    Vec<DuplicateProviderFile>,
)> {
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

    let outcomes: Vec<Result<CurseForgeFileRelease, ModPlatformError>> =
        futures_util::stream::iter(
            distinct
                .iter()
                .map(|(project, file)| operation.file(*project, *file, cancel)),
        )
        .buffered(LOOKUP_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    // Every resolved release, restricted or not: the one-file-per-project
    // choice (ADR 0031) must see a restricted duplicate too, or a discarded
    // copy would still be installed from the cache or requested by hand.
    let mut by_identity: BTreeMap<(CurseForgeProjectId, CurseForgeFileId), CurseForgeFileRelease> =
        BTreeMap::new();
    let mut blocked_reasons: BTreeMap<(CurseForgeProjectId, CurseForgeFileId), BlockedFileDetail> =
        BTreeMap::new();
    for ((project, file), outcome) in distinct.iter().copied().zip(outcomes) {
        let release = outcome.map_err(CoreError::from)?;
        if release.artifact.distribution.is_restricted() {
            let classified = classes.get(&project);
            blocked_reasons.insert(
                (project, file),
                BlockedFileDetail {
                    project_name: classified.map(|classified| classified.name.clone()),
                    file_name: Some(release.artifact.file_name.clone()),
                    // The exact file's own download link, not just the
                    // project's page: CurseForge starts this file's download
                    // automatically rather than requiring another click on
                    // the file's info page.
                    browser_url: classified.and_then(|classified| {
                        classified
                            .browser_path_segment
                            .map(|segment| file_browser_url(segment, &classified.slug, file))
                    }),
                    hashes: release.artifact.hashes.clone(),
                    relations: crate::mods::recipe_relations(&release.dependencies),
                },
            );
        }
        by_identity.insert((project, file), release);
    }

    let (winners, duplicates) =
        resolve_duplicate_projects(&by_identity, classes, minecraft_version);
    let (resolved, blocked) = assemble_resolved_files(
        pending,
        &wanted,
        &by_identity,
        &blocked_reasons,
        &winners,
        classes,
        hosts,
    )?;
    Ok((resolved, blocked, duplicates))
}

/// Turns each pending entry into a recipe entry or a blocked entry, skipping
/// any a duplicate project (see [`resolve_duplicate_projects`]) already
/// decided against.
fn assemble_resolved_files(
    pending: &[PendingProviderFile],
    wanted: &[(CurseForgeProjectId, CurseForgeFileId)],
    by_identity: &BTreeMap<(CurseForgeProjectId, CurseForgeFileId), CurseForgeFileRelease>,
    blocked_reasons: &BTreeMap<(CurseForgeProjectId, CurseForgeFileId), BlockedFileDetail>,
    winners: &BTreeMap<CurseForgeProjectId, CurseForgeFileId>,
    classes: &BTreeMap<CurseForgeProjectId, ClassifiedProject>,
    hosts: &DomainAllowlist,
) -> CoreResult<(Vec<PackFileRecipe>, Vec<BlockedProviderFile>)> {
    let mut resolved = Vec::with_capacity(by_identity.len());
    let mut blocked = Vec::with_capacity(blocked_reasons.len());
    for (entry, identity) in pending.iter().zip(wanted) {
        if winners.get(&identity.0) != Some(&identity.1) {
            // Superseded by a better candidate for the same project; already
            // named in `resolve_duplicate_projects`'s report.
            continue;
        }
        if let Some(detail) = blocked_reasons.get(identity) {
            blocked.push(BlockedProviderFile {
                identity: provider_identity(identity.0, identity.1),
                project_name: detail.project_name.clone(),
                file_name: detail.file_name.clone(),
                browser_url: detail.browser_url.clone(),
                hashes: detail.hashes.clone(),
                relations: Some(detail.relations.clone()),
                client: entry.client,
                server: entry.server,
            });
        } else if let Some(release) = by_identity.get(identity) {
            let content_class = classes
                .get(&identity.0)
                .map(|classified| classified.content)
                // Unreachable: `classes` was built from this same `pending`
                // list's projects, so every one of them has an entry.
                .ok_or_else(|| {
                    CoreError::from(ModPlatformError::ResponseInvalid {
                        provider: ModProvider::CurseForge,
                        detail: format!(
                            "project {} was resolved without being classified first",
                            identity.0
                        ),
                    })
                })?;
            resolved.push(recipe_entry(entry, release, content_class, hosts)?);
        } else {
            // Unreachable: every distinct identity was requested and landed
            // in the release map.
            return Err(CoreError::from(ModPlatformError::FileUnknown {
                provider: ModProvider::CurseForge,
                project_id: identity.0.to_string(),
                file_id: identity.1.to_string(),
            }));
        }
    }
    Ok((resolved, blocked))
}

/// Groups every resolved file by project, picks one winner per project (see
/// [`pick_winner`]), and reports every project where there was more than one
/// candidate to choose from.
///
/// `by_identity`'s keys iterate sorted by `(project, file)`, so files for the
/// same project are already adjacent and are grouped with [`slice::chunk_by`]
/// rather than a second map.
fn resolve_duplicate_projects(
    by_identity: &BTreeMap<(CurseForgeProjectId, CurseForgeFileId), CurseForgeFileRelease>,
    classes: &BTreeMap<CurseForgeProjectId, ClassifiedProject>,
    minecraft_version: &str,
) -> (
    BTreeMap<CurseForgeProjectId, CurseForgeFileId>,
    Vec<DuplicateProviderFile>,
) {
    let identities: Vec<(CurseForgeProjectId, CurseForgeFileId)> =
        by_identity.keys().copied().collect();

    let mut winners = BTreeMap::new();
    let mut duplicates = Vec::new();
    for group in identities.chunk_by(|left, right| left.0 == right.0) {
        let project = group[0].0;
        let files: Vec<CurseForgeFileId> = group.iter().map(|&(_, file)| file).collect();
        let winner = pick_winner(
            project,
            files[0],
            &files[1..],
            by_identity,
            minecraft_version,
        );
        let kept = &by_identity[&(project, winner)];
        let reason = if kept
            .game_versions
            .iter()
            .any(|version| version == minecraft_version)
        {
            DuplicateFileReason::MatchesMinecraftVersion
        } else {
            DuplicateFileReason::MoreRecentlyPublished
        };
        let project_name = classes
            .get(&project)
            .map(|classified| classified.name.clone());
        for &file in &files {
            if file != winner {
                let discarded = &by_identity[&(project, file)];
                duplicates.push(DuplicateProviderFile {
                    kept: provider_identity(project, winner),
                    discarded: provider_identity(project, file),
                    project_name: project_name.clone(),
                    kept_file_name: Some(kept.artifact.file_name.clone()),
                    discarded_file_name: Some(discarded.artifact.file_name.clone()),
                    reason,
                });
            }
        }
        winners.insert(project, winner);
    }
    (winners, duplicates)
}

/// The identity a resolved CurseForge project/file pair records, for a
/// caller that needs the pair as a [`ProviderFileIdentity`] rather than as
/// the raw ids.
fn provider_identity(project: CurseForgeProjectId, file: CurseForgeFileId) -> ProviderFileIdentity {
    ProviderFileIdentity {
        provider: Some(ModProvider::CurseForge),
        project_id: Some(project.to_string()),
        version_id: Some(file.to_string()),
        file_id: Some(file.to_string()),
    }
}

/// Which of one project's several resolved candidate files to keep.
///
/// Preferred: the file whose own `gameVersions` names the pack's exact
/// Minecraft version - the pack's declared target is a fact this launcher
/// already has, and a file that fits it is a better answer than a guess from
/// publish dates alone. When none, more than one, or exactly one candidate
/// matches equally, the more recently published file wins, mirroring how the
/// mod browser already orders versions by [`CurseForgeFileRelease::published_at`].
/// A tie keeps whichever of the two was already winning.
fn pick_winner(
    project: CurseForgeProjectId,
    first: CurseForgeFileId,
    rest: &[CurseForgeFileId],
    by_identity: &BTreeMap<(CurseForgeProjectId, CurseForgeFileId), CurseForgeFileRelease>,
    minecraft_version: &str,
) -> CurseForgeFileId {
    let release_of = |file: CurseForgeFileId| &by_identity[&(project, file)];
    let matches_target = |file: CurseForgeFileId| {
        release_of(file)
            .game_versions
            .iter()
            .any(|version| version == minecraft_version)
    };
    let beats = |candidate: CurseForgeFileId, current: CurseForgeFileId| {
        matches_target(candidate)
            .cmp(&matches_target(current))
            .then(
                release_of(candidate)
                    .published_at
                    .cmp(&release_of(current).published_at),
            )
            == std::cmp::Ordering::Greater
    };
    rest.iter().copied().fold(first, |best, candidate| {
        if beats(candidate, best) {
            candidate
        } else {
            best
        }
    })
}

/// A human-readable line naming a skipped file, for
/// [`AttentionReason::ManualDownloadRequired`].
///
/// Falls back to the raw ids when a name is unavailable, the same way the
/// blocked-file listing itself does: an id is still durable and specific,
/// just less readable than a name.
fn manual_download_detail(blocked: &BlockedProviderFile) -> String {
    let project = blocked
        .project_name
        .as_deref()
        .or(blocked.identity.project_id.as_deref())
        .unwrap_or("unknown project");
    let file = blocked
        .file_name
        .as_deref()
        .or(blocked.identity.file_id.as_deref())
        .unwrap_or("unknown file");
    format!("{project} - {file}")
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
/// The destination directory comes from the project's confirmed class, never
/// from the file's own name or extension: a `.zip` is not evidence that a
/// project is a Resource Pack, and a provider-declared Resource Pack is never
/// routed as though it were a mod.
///
/// The two sides come from two different places, and that is deliberate
/// rather than an inconsistency. The client side is pack inclusion, exactly
/// as the manifest stated it: `required` names the client's desired state,
/// and nothing about a project's own environment overrides that choice. The
/// server side has no such flag to read, so it comes from what this exact
/// file's `gameVersions` says about itself; a file that names neither side
/// stays [`SideRequirement::Unknown`], which is still the honest answer for a
/// CurseForge author who declared nothing.
fn recipe_entry(
    entry: &PendingProviderFile,
    release: &CurseForgeFileRelease,
    content_class: PackContentClass,
    hosts: &DomainAllowlist,
) -> CoreResult<PackFileRecipe> {
    // The file name comes from a provider and is untrusted: it is validated as
    // a single safe path component before it can name a destination, the same
    // way regardless of which directory it is about to be joined onto.
    let name = ephervex_pack::safe_file_name(&release.artifact.file_name).ok_or_else(|| {
        PackError::EntryPathUnsafe {
            entry: release.artifact.file_name.clone(),
        }
    })?;

    // The address is provider data, and provider data decides which machine the
    // launcher connects to. It is checked like any other remote address, and a
    // refusal is fatal: there is no second address for a CurseForge file. A
    // file the author forbids third-party downloads of never reaches here -
    // it is collected as a blocked file instead.
    let address = release
        .artifact
        .distribution
        .download_url()
        .map(|url| {
            hosts
                .validate(url)
                .map_err(|source| PackError::DownloadUrlRejected {
                    path: name.to_string_lossy().into_owned(),
                    source,
                })
        })
        .transpose()?;

    Ok(PackFileRecipe {
        logical_path: PathBuf::from(content_class.logical_dir()).join(name),
        hashes: release.artifact.hashes.clone(),
        downloads: address.map(|url| url.to_string()).into_iter().collect(),
        size: release.artifact.size,
        client: entry.client,
        server: crate::mods::side_requirement(release.server_side),
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
        original_identity: None,
        relations: Some(crate::mods::recipe_relations(&release.dependencies)),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::io::Write as _;
    use std::path::Path;

    use ephervex_model::ModRelationKind;
    use ephervex_pack::RelationTarget;

    use ephervex_auth::{CredentialStore, MemoryStore};
    use ephervex_download::{
        ArtifactCache, CacheLayout, ContentStore, FileExtension, OperationReporter,
    };
    use ephervex_model::{
        ArtifactCacheLimit, Hash, HashAlgorithm, ImportSourceKind, LoaderKind, SecretString,
        StorageClass,
    };
    use ephervex_pack::{
        OptionalSelection, PackSide, SideRequirement, SourceStore, UnknownServerFiles,
    };
    use ephervex_storage::Paths;
    use sha1::{Digest as _, Sha1};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use crate::error::CoreError;
    use crate::import::ImportRequest;
    use crate::server::ServerCreateOptions;
    use crate::server_pack::ServerContentSource;

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

    /// The same document [`file_document`] serves, but declaring an
    /// environment (`Client` and/or `Server`) in `gameVersions` - the one
    /// piece of the wire shape that decides a resolved file's own server-side
    /// requirement, as opposed to the pack's `required` flag, which decides
    /// only the client side.
    fn file_document_with_environment(
        server: &MockServer,
        project: u32,
        file: u32,
        name: &str,
        body: &[u8],
        game_versions: &[&str],
    ) -> String {
        let game_versions = game_versions
            .iter()
            .map(|entry| format!("\"{entry}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"data":{{"id":{file},"gameId":432,"modId":{project},"isAvailable":true,
                "displayName":"{name}","fileName":"{name}","releaseType":1,"fileStatus":4,
                "fileLength":{length},
                "downloadUrl":"{base}/files/{project}/{file}/{name}",
                "gameVersions":[{game_versions}],
                "hashes":[{{"value":"{digest}","algo":1}},
                          {{"value":"0123456789abcdef0123456789abcdef","algo":2}}]}}}}"#,
            length = body.len(),
            base = server.uri(),
            digest = sha1_of(body),
        )
    }

    /// The same document [`file_document_with_environment`] serves, but also
    /// declaring `fileDate` - for a test that has to tell two candidate files
    /// apart by when CurseForge published them.
    fn file_document_with_game_versions_and_date(
        server: &MockServer,
        project: u32,
        file: u32,
        name: &str,
        body: &[u8],
        game_versions: &[&str],
        file_date: &str,
    ) -> String {
        let game_versions = game_versions
            .iter()
            .map(|entry| format!("\"{entry}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"data":{{"id":{file},"gameId":432,"modId":{project},"isAvailable":true,
                "displayName":"{name}","fileName":"{name}","releaseType":1,"fileStatus":4,
                "fileLength":{length},
                "downloadUrl":"{base}/files/{project}/{file}/{name}",
                "gameVersions":[{game_versions}],
                "fileDate":"{file_date}",
                "hashes":[{{"value":"{digest}","algo":1}},
                          {{"value":"0123456789abcdef0123456789abcdef","algo":2}}]}}}}"#,
            length = body.len(),
            base = server.uri(),
            digest = sha1_of(body),
        )
    }

    /// Serves one exact file's metadata (with declared Minecraft versions and
    /// a publish date) and its bytes.
    async fn serve_file_with_game_versions_and_date(
        server: &MockServer,
        project: u32,
        file: u32,
        name: &str,
        body: &[u8],
        game_versions: &[&str],
        file_date: &str,
    ) {
        Mock::given(method("GET"))
            .and(path(format!("/v1/mods/{project}/files/{file}")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                file_document_with_game_versions_and_date(
                    server,
                    project,
                    file,
                    name,
                    body,
                    game_versions,
                    file_date,
                ),
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

    /// Serves one exact file's metadata for a file whose author forbids
    /// third-party downloads (no `downloadUrl`), with declared Minecraft
    /// versions and a publish date. Its bytes are never served.
    async fn serve_restricted_file_with_game_versions_and_date(
        server: &MockServer,
        project: u32,
        file: u32,
        name: &str,
        body: &[u8],
        game_versions: &[&str],
        file_date: &str,
    ) {
        let game_versions = game_versions
            .iter()
            .map(|version| format!("\"{version}\""))
            .collect::<Vec<_>>()
            .join(",");
        Mock::given(method("GET"))
            .and(path(format!("/v1/mods/{project}/files/{file}")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":{{"id":{file},"gameId":432,"modId":{project},
                        "isAvailable":true,"fileName":"{name}","fileLength":{length},
                        "downloadUrl":null,
                        "gameVersions":[{game_versions}],
                        "fileDate":"{file_date}",
                        "hashes":[{{"value":"{digest}","algo":1}}]}}}}"#,
                    length = body.len(),
                    digest = sha1_of(body),
                ),
                "application/json",
            ))
            .mount(server)
            .await;
    }

    /// The numeric id this test fixture's loopback server uses for the Mods
    /// class. Deliberately not `6`, the id the production API happens to use
    /// today: a test suite that only ever exercised the id validation is
    /// meant to discover would still pass if the implementation quietly
    /// started assuming a fixed number instead of discovering one.
    const MODS_CLASS_ID: u32 = 987_654;

    /// The numeric id this test fixture's loopback server uses for the
    /// Resource Packs class. Arbitrary, on the same terms as
    /// [`MODS_CLASS_ID`]: a fixture that only ever exercised discovery would
    /// still pass if the implementation started assuming this number instead.
    const RESOURCE_PACKS_CLASS_ID: u32 = 12;

    /// The numeric id this test fixture's loopback server uses for the Shader
    /// Packs class. Arbitrary, on the same terms as [`MODS_CLASS_ID`].
    const SHADER_PACKS_CLASS_ID: u32 = 19;

    /// The numeric id this test fixture's loopback server uses for the Worlds
    /// class, a real CurseForge class this build does not import and never
    /// will in this suite: it stands in for "a known but unsupported class".
    const WORLDS_CLASS_ID: u32 = 17;

    /// The documented shape of `GET /v1/categories?classesOnly=true`, naming
    /// the Mods, Resource Packs, and Shader Packs classes by their own ids
    /// and one decoy class beside them, so a test proves the right one was
    /// picked rather than the only one, and so a refusal has a real
    /// provider-returned class name available.
    async fn serve_project_classes(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/v1/categories"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":[
                        {{"id":{MODS_CLASS_ID},"gameId":432,"name":"Mods","slug":"mc-mods","isClass":true}},
                        {{"id":{RESOURCE_PACKS_CLASS_ID},"gameId":432,"name":"Resource Packs","slug":"texture-packs","isClass":true}},
                        {{"id":{SHADER_PACKS_CLASS_ID},"gameId":432,"name":"Shaders","slug":"shaders","isClass":true}},
                        {{"id":{WORLDS_CLASS_ID},"gameId":432,"name":"Worlds","slug":"worlds","isClass":true}}
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
        archive_with_manifest(dir, name, &export_manifest())
    }

    /// The same fixture archive, with its own manifest, for a test that needs
    /// a shape [`export_manifest`] does not declare.
    fn archive_with_manifest(dir: &Path, name: &str, manifest: &[u8]) -> std::path::PathBuf {
        let archive = dir.join(name);
        zip_file(
            &archive,
            &[
                ("manifest.json", manifest),
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
    const RESOURCE_PACK_BODY: &[u8] = b"the resource pack zip";
    const RESOURCE_PACK_FILE_NAME: &str = "faithful-32x.zip";
    const SHADER_PACK_BODY: &[u8] = b"the shader pack zip";
    const SHADER_PACK_FILE_NAME: &str = "complementary-r5.zip";

    /// A manifest declaring [`REQUIRED_PROJECT`] and, also required so it
    /// lands on disk without a selection, [`OPTIONAL_PROJECT`]: reused here
    /// as the Resource Pack project rather than the optional mod
    /// [`export_manifest`] declares it as.
    fn export_manifest_with_a_required_resource_pack() -> Vec<u8> {
        export_manifest_with_files(&format!(
            r#"{{"projectID": {REQUIRED_PROJECT}, "fileID": {REQUIRED_FILE}, "required": true}},
               {{"projectID": {OPTIONAL_PROJECT}, "fileID": {OPTIONAL_FILE}, "required": true}}"#
        ))
    }

    /// The same shape, with [`OPTIONAL_PROJECT`] reused as a Shader Pack
    /// project instead.
    fn export_manifest_with_a_required_shader_pack() -> Vec<u8> {
        export_manifest_with_a_required_resource_pack()
    }

    /// Classifies [`REQUIRED_PROJECT`] as a Mod and [`OPTIONAL_PROJECT`] as a
    /// Resource Pack, and serves both of their exact declared files.
    async fn serve_pack_with_a_resource_pack(server: &MockServer) {
        serve_project_classes(server).await;
        serve_project_class(server, REQUIRED_PROJECT, "Actual Mod", MODS_CLASS_ID).await;
        serve_project_class(
            server,
            OPTIONAL_PROJECT,
            "Faithful",
            RESOURCE_PACKS_CLASS_ID,
        )
        .await;
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
            RESOURCE_PACK_FILE_NAME,
            RESOURCE_PACK_BODY,
        )
        .await;
    }

    /// The same shape as [`serve_pack_with_a_resource_pack`], classifying
    /// [`OPTIONAL_PROJECT`] as a Shader Pack instead.
    async fn serve_pack_with_a_shader_pack(server: &MockServer) {
        serve_project_classes(server).await;
        serve_project_class(server, REQUIRED_PROJECT, "Actual Mod", MODS_CLASS_ID).await;
        serve_project_class(
            server,
            OPTIONAL_PROJECT,
            "Complementary",
            SHADER_PACKS_CLASS_ID,
        )
        .await;
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
            SHADER_PACK_FILE_NAME,
            SHADER_PACK_BODY,
        )
        .await;
    }

    /// The same document [`file_document`] serves, plus the relations this file
    /// declares about other projects.
    fn file_document_with_relations(
        server: &MockServer,
        project: u32,
        file: u32,
        name: &str,
        body: &[u8],
        relations: &[(u32, u32)],
    ) -> String {
        let relations = relations
            .iter()
            .map(|(mod_id, relation_type)| {
                format!(r#"{{"modId":{mod_id},"relationType":{relation_type}}}"#)
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"data":{{"id":{file},"gameId":432,"modId":{project},"isAvailable":true,
                "displayName":"{name}","fileName":"{name}","releaseType":1,"fileStatus":4,
                "fileLength":{length},
                "downloadUrl":"{base}/files/{project}/{file}/{name}",
                "dependencies":[{relations}],
                "hashes":[{{"value":"{digest}","algo":1}},
                          {{"value":"0123456789abcdef0123456789abcdef","algo":2}}]}}}}"#,
            length = body.len(),
            base = server.uri(),
            digest = sha1_of(body),
        )
    }

    async fn serve_file_with_relations(
        server: &MockServer,
        project: u32,
        file: u32,
        name: &str,
        body: &[u8],
        relations: &[(u32, u32)],
    ) {
        Mock::given(method("GET"))
            .and(path(format!("/v1/mods/{project}/files/{file}")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                file_document_with_relations(server, project, file, name, body, relations),
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
    async fn an_imported_packs_files_record_the_relations_curseforge_declared() {
        // Two projects this pack does not itself contain: one the file does not
        // load without, one it merely suggests.
        const LIBRARY_PROJECT: u32 = 306_613;
        const SUGGESTED_PROJECT: u32 = 999_111;
        // CurseForge's own `relationType` values, as its API documents them.
        // Spelled out here rather than reached for across the crate boundary:
        // this is a fixture asserting against the wire format.
        const REQUIRED_DEPENDENCY: u32 = 3;
        const OPTIONAL_DEPENDENCY: u32 = 2;

        // The file document the import already fetches carries these. Keeping
        // them is what lets the instance answer "what breaks if this is turned
        // off" later, offline and with no key (ADR 0034).
        let server = MockServer::start().await;
        serve_pack_as_mods(&server).await;
        serve_file_with_relations(
            &server,
            REQUIRED_PROJECT,
            REQUIRED_FILE,
            "required-1.0.0.jar",
            REQUIRED_BODY,
            &[
                (LIBRARY_PROJECT, REQUIRED_DEPENDENCY),
                (SUGGESTED_PROJECT, OPTIONAL_DEPENDENCY),
            ],
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
        let imported = import(&app, &archive).await.expect("import");

        let recipe = imported.recipe();
        let required = &recipe.files[0];
        let relations = required
            .recorded_relations()
            .expect("the file document declared relations");
        assert_eq!(relations.len(), 2);
        let required_on = |project: u32| {
            relations.iter().any(|relation| {
                relation.kind.is_required()
                    && relation.target.project_id() == Some(project.to_string().as_str())
            })
        };
        assert!(
            required_on(LIBRARY_PROJECT),
            "a required relation is what a disable warning is built on"
        );
        assert!(
            !required_on(SUGGESTED_PROJECT),
            "an optional relation never becomes a requirement"
        );
        assert!(
            relations.iter().all(|relation| matches!(
                &relation.target,
                RelationTarget::Project {
                    version_id: None,
                    ..
                }
            )),
            "CurseForge states relations project to project; no file is pinned"
        );

        let optional = &recipe.files[1];
        assert_eq!(
            optional.recorded_relations(),
            Some(&[][..]),
            "a document that declares none answered, and is not unasked"
        );
    }

    #[tokio::test]
    async fn a_pinned_files_own_environment_decides_its_server_requirement() {
        // The `required` flag still decides the client side. The server side
        // is not a placeholder here: this exact file says `Client` about
        // itself, and that is what the recipe must record for the server.
        let server = MockServer::start().await;
        serve_pack_as_mods(&server).await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/{REQUIRED_PROJECT}/files/{REQUIRED_FILE}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                file_document_with_environment(
                    &server,
                    REQUIRED_PROJECT,
                    REQUIRED_FILE,
                    "required-1.0.0.jar",
                    REQUIRED_BODY,
                    &["Client", "1.20.1", "Forge"],
                ),
                "application/json",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/files/{REQUIRED_PROJECT}/{REQUIRED_FILE}/required-1.0.0.jar"
            )))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(REQUIRED_BODY.to_vec(), "application/java-archive"),
            )
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

        let imported = import(&app, &archive).await.expect("import");
        let recipe = imported.recipe();
        let required = &recipe.files[0];
        assert_eq!(required.client, SideRequirement::Required);
        assert_eq!(
            required.server,
            SideRequirement::Unsupported,
            "the file names Client and not Server, which is a claim to be client-only"
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
        // `classify_projects` is exercised directly rather than through a
        // full manifest: the pack parser already collapses two declared
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

        classify_projects(&operation, &pending, &CancellationToken::new())
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
    async fn a_known_unsupported_class_refuses_the_whole_pack_import_in_pack_context() {
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", MODS_CLASS_ID).await;
        // A real, known CurseForge class this build still does not import: a
        // World, wearing the "Worlds" class from serve_project_classes's own
        // fixture. The project's own display name is deliberately unrelated
        // to its class, so a rejection that named the *project* instead of
        // the *class* would be caught rather than passing by coincidence.
        serve_project_class(&server, OPTIONAL_PROJECT, "A Neat World", WORLDS_CLASS_ID).await;
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
        // The pack-import refusal, not the mod browser's "not a mod" wording:
        // a modpack legitimately contains more than mods.
        assert_eq!(
            error.message_id(),
            "error-pack-provider-content-unsupported"
        );
        assert_eq!(
            error.message_args(),
            vec![
                ("provider", "CurseForge".to_owned()),
                ("reference", OPTIONAL_PROJECT.to_string()),
                ("project_type", "Worlds".to_owned()),
            ],
            "the refusal must name the class the categories response returned, \
             never the unrelated project's own display name"
        );
        assert!(
            app.list_clients().expect("list").is_empty(),
            "a pack with one unsupported-class project publishes no instance at all, \
             not a partial one missing that entry"
        );

        // The file endpoint for the rejected project's own file was never
        // asked: rejection happens before any file is resolved.
        let recorded = server.received_requests().await.expect("recorded");
        assert!(
            recorded
                .iter()
                .all(|request| !request.url.path().contains(&OPTIONAL_FILE.to_string())),
            "classification must refuse before any file lookup, not skip the entry it refused"
        );
    }

    #[tokio::test]
    async fn an_optional_entrys_project_must_also_be_an_importable_class() {
        // The required project is a legitimate mod; only the optional entry
        // names a class this build does not import. Optional inclusion does
        // not exempt a project from classification, even though the pack
        // importer now accepts more than one class overall.
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", MODS_CLASS_ID).await;
        serve_project_class(&server, OPTIONAL_PROJECT, "A World", WORLDS_CLASS_ID).await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(
            error.message_id(),
            "error-pack-provider-content-unsupported"
        );
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

    // -- resource packs ------------------------------------------------------

    #[tokio::test]
    async fn resource_packs_class_discovery_does_not_depend_on_any_fixed_numeric_id() {
        // A second server whose Mods and Resource Packs classes both carry
        // different unusual ids than the shared fixture's, so this test
        // cannot pass by accident on whatever numbers the other tests share.
        const OTHER_MODS_CLASS_ID: u32 = 444_222;
        const OTHER_RESOURCE_PACKS_CLASS_ID: u32 = 333_777;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/categories"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":[
                        {{"id":{OTHER_RESOURCE_PACKS_CLASS_ID},"gameId":432,"name":"Resource Packs","slug":"texture-packs","isClass":true}},
                        {{"id":{OTHER_MODS_CLASS_ID},"gameId":432,"name":"Mods","slug":"mc-mods","isClass":true}}
                    ]}}"#
                ),
                "application/json",
            ))
            .mount(&server)
            .await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", OTHER_MODS_CLASS_ID).await;
        serve_project_class(
            &server,
            OPTIONAL_PROJECT,
            "Faithful",
            OTHER_RESOURCE_PACKS_CLASS_ID,
        )
        .await;
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
            RESOURCE_PACK_FILE_NAME,
            RESOURCE_PACK_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_with_manifest(
            dir.path(),
            "repro.zip",
            &export_manifest_with_a_required_resource_pack(),
        );
        let app = app_against(dir.path(), &server);

        let imported = import(&app, &archive).await.expect(
            "the Resource Packs class this server discovers is not the one every other test's fixture uses",
        );
        let game = ephervex_storage::metadata::client_game_dir(&imported.record.root);
        assert_eq!(
            std::fs::read(game.join("resourcepacks").join(RESOURCE_PACK_FILE_NAME)).expect("read"),
            RESOURCE_PACK_BODY,
            "routed correctly even though this server's Resource Packs class id \
             differs from every other test's"
        );
    }

    #[tokio::test]
    async fn a_manifest_resource_pack_is_imported_and_routed_to_resourcepacks() {
        let server = MockServer::start().await;
        serve_pack_with_a_resource_pack(&server).await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_with_manifest(
            dir.path(),
            "repro.zip",
            &export_manifest_with_a_required_resource_pack(),
        );
        let app = app_against(dir.path(), &server);

        let imported = import(&app, &archive).await.expect("import");
        let game = ephervex_storage::metadata::client_game_dir(&imported.record.root);
        assert_eq!(
            std::fs::read(game.join("mods/required-1.0.0.jar")).expect("read"),
            REQUIRED_BODY
        );
        assert_eq!(
            std::fs::read(game.join("resourcepacks").join(RESOURCE_PACK_FILE_NAME)).expect("read"),
            RESOURCE_PACK_BODY,
            "a confirmed Resource Pack project routes to resourcepacks/, never mods/"
        );

        let recipe = imported.recipe();
        assert_eq!(recipe.files.len(), 2);
        let resource_pack = recipe
            .files
            .iter()
            .find(|file| {
                file.logical_path == Path::new("resourcepacks").join(RESOURCE_PACK_FILE_NAME)
            })
            .expect("the resource pack is in the recipe");
        assert_eq!(resource_pack.origin, ModOrigin::PackDeclared);
        assert_eq!(resource_pack.client, SideRequirement::Required);
        assert_eq!(
            resource_pack.server,
            SideRequirement::Unknown,
            "the export format states nothing about servers, resource packs included"
        );
        let identity = resource_pack.identity.as_ref().expect("an identity");
        assert_eq!(identity.provider, Some(ModProvider::CurseForge));
        assert_eq!(identity.project_id.as_deref(), Some("306612"));
        assert_eq!(
            identity.file_id.as_deref(),
            Some("4593600"),
            "the exact manifest file id survives resolution"
        );
        assert_eq!(identity.version_id.as_deref(), Some("4593600"));
        assert!(
            !resource_pack.hashes.is_empty(),
            "a provider-published digest survives resolution and stays verifiable"
        );

        // Not a managed mod: fully identified, and still never surfaced by
        // either the resolver-facing list or the frontend-facing snapshot.
        let managed = recipe.managed_mods();
        assert_eq!(managed.len(), 1, "only the actual mod is a managed mod");
        assert_eq!(managed[0].project_id, "238222");
        assert!(
            managed.iter().all(|mod_| mod_.project_id != "306612"),
            "the resource pack must never appear as a managed mod: {managed:?}"
        );

        let snapshot = app
            .managed_mod_snapshot(imported.record.id)
            .expect("snapshot");
        assert_eq!(
            snapshot.mods.len(),
            1,
            "the resource pack is not part of the managed-mod snapshot either"
        );
    }

    #[tokio::test]
    async fn an_optional_manifest_entry_may_declare_a_resource_pack() {
        // The manifest's own required/optional semantics are unaffected by a
        // project's class: an optional Resource Pack is recorded and left
        // uninstalled by default, exactly like an optional mod.
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", MODS_CLASS_ID).await;
        serve_project_class(
            &server,
            OPTIONAL_PROJECT,
            "Faithful",
            RESOURCE_PACKS_CLASS_ID,
        )
        .await;
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
            RESOURCE_PACK_FILE_NAME,
            RESOURCE_PACK_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        // The unmodified fixture manifest: REQUIRED_PROJECT is required,
        // OPTIONAL_PROJECT is `required: false`.
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let imported = import(&app, &archive).await.expect("import");
        let game = ephervex_storage::metadata::client_game_dir(&imported.record.root);
        assert!(
            !game
                .join("resourcepacks")
                .join(RESOURCE_PACK_FILE_NAME)
                .exists(),
            "an optional resource pack nobody selected is not installed, same as an optional mod"
        );

        let recipe = imported.recipe();
        let resource_pack = recipe
            .files
            .iter()
            .find(|file| {
                file.logical_path == Path::new("resourcepacks").join(RESOURCE_PACK_FILE_NAME)
            })
            .expect("the optional resource pack is still recorded in the recipe");
        assert_eq!(resource_pack.client, SideRequirement::Optional);
        assert_eq!(
            resource_pack.selected,
            Some(false),
            "the user was asked and said no, the same default an optional mod gets"
        );
    }

    #[tokio::test]
    async fn a_retained_resource_pack_reconstructs_under_resourcepacks_on_a_server() {
        let server = MockServer::start().await;
        serve_pack_with_a_resource_pack(&server).await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_with_manifest(
            dir.path(),
            "repro.zip",
            &export_manifest_with_a_required_resource_pack(),
        );
        let app = app_against(dir.path(), &server);
        let imported = import(&app, &archive).await.expect("import");

        // Nothing below this line may contact anyone: reconstruction works
        // from the retained recipe and the shared artifact cache alone.
        drop(server);

        let target = reconstruct_server_confirming(
            &app,
            &imported,
            &dir.path().join("server"),
            UnknownServerFiles::only(OptionalSelection::of([
                "mods/required-1.0.0.jar".to_owned(),
                format!("resourcepacks/{RESOURCE_PACK_FILE_NAME}"),
            ])),
        )
        .await
        .expect("reconstruction");

        assert_eq!(
            std::fs::read(target.join("resourcepacks").join(RESOURCE_PACK_FILE_NAME))
                .expect("read"),
            RESOURCE_PACK_BODY,
            "the retained resource pack reconstructs back under resourcepacks/"
        );
    }

    #[tokio::test]
    async fn an_unclassified_projects_zip_shaped_name_is_not_treated_as_a_provider_pack() {
        // A project reporting no class at all, whose title and slug are
        // deliberately shaped like a resource pack's or a shader pack's.
        // None of that is evidence: only a confirmed `classId` is.
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", MODS_CLASS_ID).await;
        serve_project_with_no_class(&server, OPTIONAL_PROJECT, "Complementary Faithful.zip").await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-mod-provider-response-invalid");
        assert!(
            !matches!(
                error,
                CoreError::Pack(PackError::ProviderContentUnsupported { .. })
            ),
            "a null class is not a confirmed Resource Pack, Shader Pack, or any other \
             confirmed class: {error:?}"
        );
        assert!(app.list_clients().expect("list").is_empty());

        // The name never made classification look at a file at all.
        let recorded = server.received_requests().await.expect("recorded");
        assert!(
            recorded
                .iter()
                .all(|request| !request.url.path().contains(&OPTIONAL_FILE.to_string())),
            "a `.zip`-shaped title must not substitute for a confirmed class"
        );
    }

    #[tokio::test]
    async fn an_unsafe_resource_pack_file_name_cannot_escape_resourcepacks() {
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", MODS_CLASS_ID).await;
        serve_project_class(
            &server,
            OPTIONAL_PROJECT,
            "Faithful",
            RESOURCE_PACKS_CLASS_ID,
        )
        .await;
        serve_file(
            &server,
            REQUIRED_PROJECT,
            REQUIRED_FILE,
            "required-1.0.0.jar",
            REQUIRED_BODY,
        )
        .await;
        // A confirmed Resource Pack whose provider-published file name is not
        // a single safe path component. The same validation every provider
        // file name gets, regardless of which directory it would land in.
        serve_file(
            &server,
            OPTIONAL_PROJECT,
            OPTIONAL_FILE,
            "../escape.zip",
            RESOURCE_PACK_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-pack-entry-unsafe");
        assert!(app.list_clients().expect("list").is_empty());
    }

    #[tokio::test]
    async fn a_restricted_resource_pack_file_refuses_the_whole_import_atomically() {
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", MODS_CLASS_ID).await;
        serve_project_class(
            &server,
            OPTIONAL_PROJECT,
            "Faithful",
            RESOURCE_PACKS_CLASS_ID,
        )
        .await;
        serve_file(
            &server,
            REQUIRED_PROJECT,
            REQUIRED_FILE,
            "required-1.0.0.jar",
            REQUIRED_BODY,
        )
        .await;
        // A confirmed Resource Pack whose author disabled third-party
        // downloads: the same conservative refusal a restricted mod gets,
        // never a substitution and never an omission.
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/{OPTIONAL_PROJECT}/files/{OPTIONAL_FILE}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":{{"id":{OPTIONAL_FILE},"gameId":432,"modId":{OPTIONAL_PROJECT},
                        "isAvailable":true,"fileName":"restricted.zip","fileLength":10,
                        "downloadUrl":null,
                        "hashes":[{{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1}}]}}}}"#
                ),
                "application/json",
            ))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-pack-provider-files-blocked");
        assert!(app.list_clients().expect("list").is_empty());
        let fetches = server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .filter(|request| request.url.path().starts_with("/files/"))
            .count();
        assert_eq!(
            fetches, 0,
            "nothing went looking for the file anywhere else"
        );
    }

    // -- shader packs ---------------------------------------------------------
    //
    // The same routing architecture as resource packs, exercised more lightly:
    // the invariants (fail-closed discovery, exact-file safety, atomicity) are
    // already proven generically above, so these confirm the third destination
    // without re-proving each of those from scratch.

    #[tokio::test]
    async fn shader_packs_class_discovery_does_not_depend_on_any_fixed_numeric_id() {
        // A server whose Mods and Shader Packs classes both carry different
        // unusual ids than the shared fixture's.
        const OTHER_MODS_CLASS_ID: u32 = 222_444;
        const OTHER_SHADER_PACKS_CLASS_ID: u32 = 777_333;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/categories"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":[
                        {{"id":{OTHER_SHADER_PACKS_CLASS_ID},"gameId":432,"name":"Shaders","slug":"shaders","isClass":true}},
                        {{"id":{OTHER_MODS_CLASS_ID},"gameId":432,"name":"Mods","slug":"mc-mods","isClass":true}}
                    ]}}"#
                ),
                "application/json",
            ))
            .mount(&server)
            .await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", OTHER_MODS_CLASS_ID).await;
        serve_project_class(
            &server,
            OPTIONAL_PROJECT,
            "Complementary",
            OTHER_SHADER_PACKS_CLASS_ID,
        )
        .await;
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
            SHADER_PACK_FILE_NAME,
            SHADER_PACK_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_with_manifest(
            dir.path(),
            "repro.zip",
            &export_manifest_with_a_required_shader_pack(),
        );
        let app = app_against(dir.path(), &server);

        let imported = import(&app, &archive).await.expect(
            "the Shader Packs class this server discovers is not the one every other test's fixture uses",
        );
        let game = ephervex_storage::metadata::client_game_dir(&imported.record.root);
        assert_eq!(
            std::fs::read(game.join("shaderpacks").join(SHADER_PACK_FILE_NAME)).expect("read"),
            SHADER_PACK_BODY,
            "routed correctly even though this server's Shader Packs class id \
             differs from every other test's"
        );
    }

    #[tokio::test]
    async fn a_manifest_shader_pack_is_imported_and_routed_to_shaderpacks() {
        let server = MockServer::start().await;
        serve_pack_with_a_shader_pack(&server).await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_with_manifest(
            dir.path(),
            "repro.zip",
            &export_manifest_with_a_required_shader_pack(),
        );
        let app = app_against(dir.path(), &server);

        let imported = import(&app, &archive).await.expect("import");
        let game = ephervex_storage::metadata::client_game_dir(&imported.record.root);
        assert_eq!(
            std::fs::read(game.join("mods/required-1.0.0.jar")).expect("read"),
            REQUIRED_BODY
        );
        assert_eq!(
            std::fs::read(game.join("shaderpacks").join(SHADER_PACK_FILE_NAME)).expect("read"),
            SHADER_PACK_BODY,
            "a confirmed Shader Pack project routes to shaderpacks/, never mods/"
        );

        let recipe = imported.recipe();
        assert_eq!(recipe.files.len(), 2);
        let shader_pack = recipe
            .files
            .iter()
            .find(|file| file.logical_path == Path::new("shaderpacks").join(SHADER_PACK_FILE_NAME))
            .expect("the shader pack is in the recipe");
        assert_eq!(shader_pack.origin, ModOrigin::PackDeclared);
        assert_eq!(shader_pack.client, SideRequirement::Required);
        assert_eq!(
            shader_pack.server,
            SideRequirement::Unknown,
            "the export format states nothing about servers, shader packs included"
        );
        let identity = shader_pack.identity.as_ref().expect("an identity");
        assert_eq!(identity.provider, Some(ModProvider::CurseForge));
        assert_eq!(identity.project_id.as_deref(), Some("306612"));
        assert_eq!(
            identity.file_id.as_deref(),
            Some("4593600"),
            "the exact manifest file id survives resolution"
        );
        assert!(
            !shader_pack.hashes.is_empty(),
            "a provider-published digest survives resolution and stays verifiable"
        );

        // Not a managed mod, fully identified or not: neither the
        // resolver-facing list nor the frontend-facing snapshot see it.
        let managed = recipe.managed_mods();
        assert_eq!(managed.len(), 1, "only the actual mod is a managed mod");
        assert!(
            managed.iter().all(|mod_| mod_.project_id != "306612"),
            "the shader pack must never appear as a managed mod: {managed:?}"
        );
        let snapshot = app
            .managed_mod_snapshot(imported.record.id)
            .expect("snapshot");
        assert_eq!(
            snapshot.mods.len(),
            1,
            "the shader pack is not part of the managed-mod snapshot either"
        );
    }

    #[tokio::test]
    async fn a_retained_shader_pack_reconstructs_under_shaderpacks_on_a_server() {
        let server = MockServer::start().await;
        serve_pack_with_a_shader_pack(&server).await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_with_manifest(
            dir.path(),
            "repro.zip",
            &export_manifest_with_a_required_shader_pack(),
        );
        let app = app_against(dir.path(), &server);
        let imported = import(&app, &archive).await.expect("import");

        // Nothing below this line may contact anyone: reconstruction works
        // from the retained recipe and the shared artifact cache alone.
        drop(server);

        let target = reconstruct_server_confirming(
            &app,
            &imported,
            &dir.path().join("server"),
            UnknownServerFiles::only(OptionalSelection::of([
                "mods/required-1.0.0.jar".to_owned(),
                format!("shaderpacks/{SHADER_PACK_FILE_NAME}"),
            ])),
        )
        .await
        .expect("reconstruction");

        assert_eq!(
            std::fs::read(target.join("shaderpacks").join(SHADER_PACK_FILE_NAME)).expect("read"),
            SHADER_PACK_BODY,
            "the retained shader pack reconstructs back under shaderpacks/"
        );
    }

    #[tokio::test]
    async fn an_unsafe_shader_pack_file_name_cannot_escape_shaderpacks() {
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", MODS_CLASS_ID).await;
        serve_project_class(
            &server,
            OPTIONAL_PROJECT,
            "Complementary",
            SHADER_PACKS_CLASS_ID,
        )
        .await;
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
            "../escape.zip",
            SHADER_PACK_BODY,
        )
        .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-pack-entry-unsafe");
        assert!(app.list_clients().expect("list").is_empty());
    }

    #[tokio::test]
    async fn a_restricted_shader_pack_file_refuses_the_whole_import_atomically() {
        let server = MockServer::start().await;
        serve_project_classes(&server).await;
        serve_project_class(&server, REQUIRED_PROJECT, "Actual Mod", MODS_CLASS_ID).await;
        serve_project_class(
            &server,
            OPTIONAL_PROJECT,
            "Complementary",
            SHADER_PACKS_CLASS_ID,
        )
        .await;
        serve_file(
            &server,
            REQUIRED_PROJECT,
            REQUIRED_FILE,
            "required-1.0.0.jar",
            REQUIRED_BODY,
        )
        .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/{OPTIONAL_PROJECT}/files/{OPTIONAL_FILE}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":{{"id":{OPTIONAL_FILE},"gameId":432,"modId":{OPTIONAL_PROJECT},
                        "isAvailable":true,"fileName":"restricted.zip","fileLength":10,
                        "downloadUrl":null,
                        "hashes":[{{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1}}]}}}}"#
                ),
                "application/json",
            ))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let error = import(&app, &archive).await.expect_err("must refuse");
        assert_eq!(error.message_id(), "error-pack-provider-files-blocked");
        assert!(app.list_clients().expect("list").is_empty());
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
                        "downloadUrl":null,
                        "hashes":[{{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1}}]}}}}"#
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
        assert_eq!(error.message_id(), "error-pack-provider-files-blocked");
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
    async fn a_blocked_file_does_not_abort_a_pack_whose_other_files_still_resolve() {
        let server = MockServer::start().await;
        serve_pack_as_mods(&server).await;
        serve_file(
            &server,
            REQUIRED_PROJECT,
            REQUIRED_FILE,
            "required-1.0.0.jar",
            REQUIRED_BODY,
        )
        .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/{OPTIONAL_PROJECT}/files/{OPTIONAL_FILE}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":{{"id":{OPTIONAL_FILE},"gameId":432,"modId":{OPTIONAL_PROJECT},
                        "isAvailable":true,"fileName":"restricted.jar","fileLength":10,
                        "downloadUrl":null,
                        "dependencies":[{{"modId":{REQUIRED_PROJECT},"relationType":3}}],
                        "hashes":[{{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1}}]}}}}"#
                ),
                "application/json",
            ))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let mut analysis = app.analyze_pack(&archive).expect("analyzed locally");
        app.resolve_pack(&mut analysis, &[], &CancellationToken::new())
            .await
            .expect("resolving collects the blocked file rather than aborting");

        assert_eq!(analysis.blocked.len(), 1);
        let blocked = &analysis.blocked[0];
        assert_eq!(blocked.project_name.as_deref(), Some("Optional Mod"));
        assert_eq!(blocked.file_name.as_deref(), Some("restricted.jar"));
        // Refusing to serve the bytes is not refusing to describe the file.
        assert_eq!(
            blocked.relations.as_deref(),
            Some(
                [RecipeRelation {
                    kind: ModRelationKind::Required,
                    target: RelationTarget::Project {
                        project_id: REQUIRED_PROJECT.to_string(),
                        version_id: None,
                    },
                }]
                .as_slice()
            ),
        );
        assert_eq!(
            blocked.browser_url.as_deref(),
            Some("https://www.curseforge.com/minecraft/mc-mods/Optional Mod/download/4593600")
        );
        assert!(
            analysis
                .recipe
                .files
                .iter()
                .any(|file| file.logical_path.ends_with("required-1.0.0.jar")),
            "the required file still resolved despite the other one being blocked"
        );
    }

    #[tokio::test]
    async fn a_blocked_file_already_verified_in_the_shared_cache_resolves_without_being_skipped() {
        const RESTRICTED_BODY: &[u8] = b"a copy the user placed by hand";

        let server = MockServer::start().await;
        serve_pack_as_mods(&server).await;
        serve_file(
            &server,
            REQUIRED_PROJECT,
            REQUIRED_FILE,
            "required-1.0.0.jar",
            REQUIRED_BODY,
        )
        .await;
        let digest = sha1_of(RESTRICTED_BODY);
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/{OPTIONAL_PROJECT}/files/{OPTIONAL_FILE}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":{{"id":{OPTIONAL_FILE},"gameId":432,"modId":{OPTIONAL_PROJECT},
                        "isAvailable":true,"fileName":"restricted.jar","fileLength":{length},
                        "downloadUrl":null,
                        "dependencies":[{{"modId":{REQUIRED_PROJECT},"relationType":3}}],
                        "hashes":[{{"value":"{digest}","algo":1}}]}}}}"#,
                    length = RESTRICTED_BODY.len(),
                ),
                "application/json",
            ))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        // A verified copy already sitting in the shared cache, as if an
        // earlier import or a folder watcher had put it there.
        let hash = Hash::new(HashAlgorithm::Sha1, &digest).expect("valid sha1");
        let extension = FileExtension::new("jar").expect("jar");
        let cache_path =
            app.artifact_cache()
                .layout()
                .content(ContentStore::Downloads, &hash, Some(&extension));
        std::fs::create_dir_all(cache_path.parent().expect("shard")).expect("shard");
        std::fs::write(&cache_path, RESTRICTED_BODY).expect("pre-populate the cache");

        let mut analysis = app.analyze_pack(&archive).expect("analyzed locally");
        app.resolve_pack(&mut analysis, &[], &CancellationToken::new())
            .await
            .expect("resolving succeeds");

        assert!(
            analysis.blocked.is_empty(),
            "a cache hit resolves the file rather than leaving it blocked"
        );
        let recovered = analysis
            .recipe
            .files
            .iter()
            .find(|file| file.logical_path.ends_with("restricted.jar"))
            .expect("the cache-recovered file is an ordinary recipe entry");
        assert!(
            recovered.downloads.is_empty(),
            "nothing needs to be fetched for a file the cache already verified"
        );
        // A download restriction must not leave the instance unable to answer
        // dependency questions about a file the catalog did describe.
        assert_eq!(
            recovered.relations.as_deref(),
            Some(
                [RecipeRelation {
                    kind: ModRelationKind::Required,
                    target: RelationTarget::Project {
                        project_id: REQUIRED_PROJECT.to_string(),
                        version_id: None,
                    },
                }]
                .as_slice()
            ),
            "recorded relation knowledge, not `None` for unknown"
        );
    }

    #[tokio::test]
    async fn skipping_a_blocked_file_lets_the_import_proceed_with_a_manual_download_attention() {
        let server = MockServer::start().await;
        serve_pack_as_mods(&server).await;
        serve_file(
            &server,
            REQUIRED_PROJECT,
            REQUIRED_FILE,
            "required-1.0.0.jar",
            REQUIRED_BODY,
        )
        .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v1/mods/{OPTIONAL_PROJECT}/files/{OPTIONAL_FILE}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"data":{{"id":{OPTIONAL_FILE},"gameId":432,"modId":{OPTIONAL_PROJECT},
                        "isAvailable":true,"fileName":"restricted.jar","fileLength":10,
                        "downloadUrl":null,
                        "hashes":[{{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1}}]}}}}"#
                ),
                "application/json",
            ))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_at(dir.path(), "repro.zip");
        let app = app_against(dir.path(), &server);

        let skip = ProviderFileIdentity {
            provider: Some(ModProvider::CurseForge),
            project_id: Some(OPTIONAL_PROJECT.to_string()),
            version_id: Some(OPTIONAL_FILE.to_string()),
            file_id: Some(OPTIONAL_FILE.to_string()),
        };
        let imported = app
            .import_pack(
                &ImportRequest::new(&archive)
                    .with_storage(StorageClass::Temporary)
                    .skipping_blocked(vec![skip]),
                &OperationReporter::silent("curseforge-import"),
                &CancellationToken::new(),
            )
            .await
            .expect("an explicit skip lets the import proceed");

        assert_eq!(
            imported.record.attention,
            vec![AttentionReason::ManualDownloadRequired(
                "Optional Mod - restricted.jar".to_owned()
            )]
        );
        assert!(
            imported
                .materialized
                .files
                .installed
                .iter()
                .all(|file| !file.ends_with("restricted.jar")),
            "a skipped file is never installed"
        );
    }

    /// Builds and serves the Garden of Glass (Questbook Edition) v4.3.4
    /// repro: Just Enough Items declared twice, a stale 1.16.4 file left over
    /// from an earlier version of the pack, and the 1.16.5 file it actually
    /// ships (ADR 0031), against a manifest that (unlike
    /// [`export_manifest_with_files`], which always declares 1.20.1) declares
    /// Minecraft 1.16.5 - the version the "matches" branch needs to see.
    async fn serve_and_declare_the_jei_duplicate(
        server: &MockServer,
        stale_file: u32,
        current_file: u32,
    ) -> Vec<u8> {
        serve_pack_as_mods(server).await;
        serve_file_with_game_versions_and_date(
            server,
            REQUIRED_PROJECT,
            stale_file,
            "jei-1.16.4-7.6.1.65.jar",
            REQUIRED_BODY,
            &["1.16.4", "Forge"],
            "2020-10-01T00:00:00.000Z",
        )
        .await;
        serve_file_with_game_versions_and_date(
            server,
            REQUIRED_PROJECT,
            current_file,
            "jei-1.16.5-7.7.0.101.jar",
            REQUIRED_BODY,
            &["1.16.5", "Forge"],
            "2021-05-01T00:00:00.000Z",
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

        format!(
            r#"{{
                "minecraft": {{
                    "version": "1.16.5",
                    "modLoaders": [{{"id": "forge-36.2.0", "primary": true}}]
                }},
                "manifestType": "minecraftModpack",
                "manifestVersion": 1,
                "name": "Repro Pack",
                "version": "4.3.4",
                "author": "A Reporter",
                "files": [
                    {{"projectID": {REQUIRED_PROJECT}, "fileID": {stale_file}, "required": true}},
                    {{"projectID": {REQUIRED_PROJECT}, "fileID": {current_file}, "required": true}},
                    {{"projectID": {OPTIONAL_PROJECT}, "fileID": {OPTIONAL_FILE}, "required": false}}
                ],
                "overrides": "overrides"
            }}"#
        )
        .into_bytes()
    }

    #[tokio::test]
    async fn a_duplicate_declaration_names_the_kept_and_discarded_files_before_import() {
        // Resolution runs before the user ever sees an "Import pack" button
        // (`spawn_import_resolution` in the GUI backend), so this is what the
        // preview - not the resulting instance - has to show.
        let server = MockServer::start().await;
        let stale_file = REQUIRED_FILE;
        let current_file = REQUIRED_FILE + 1;
        let manifest = serve_and_declare_the_jei_duplicate(&server, stale_file, current_file).await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_with_manifest(dir.path(), "repro.zip", &manifest);
        let app = app_against(dir.path(), &server);

        let mut analysis = app.analyze_pack(&archive).expect("analyzed locally");
        app.resolve_pack(&mut analysis, &[], &CancellationToken::new())
            .await
            .expect("a duplicate declaration does not fail resolution");

        assert_eq!(
            analysis.duplicate_files,
            vec![DuplicateProviderFile {
                kept: ProviderFileIdentity {
                    provider: Some(ModProvider::CurseForge),
                    project_id: Some(REQUIRED_PROJECT.to_string()),
                    version_id: Some(current_file.to_string()),
                    file_id: Some(current_file.to_string()),
                },
                discarded: ProviderFileIdentity {
                    provider: Some(ModProvider::CurseForge),
                    project_id: Some(REQUIRED_PROJECT.to_string()),
                    version_id: Some(stale_file.to_string()),
                    file_id: Some(stale_file.to_string()),
                },
                project_name: Some("Required Mod".to_owned()),
                kept_file_name: Some("jei-1.16.5-7.7.0.101.jar".to_owned()),
                discarded_file_name: Some("jei-1.16.4-7.6.1.65.jar".to_owned()),
                reason: DuplicateFileReason::MatchesMinecraftVersion,
            }],
            "the resolved names are reported, not the raw fileIDs, and say why"
        );
        assert!(
            analysis.attention.is_empty(),
            "a duplicate declaration is not an instance attention reason"
        );
    }

    #[tokio::test]
    async fn a_project_declared_twice_installs_the_file_matching_the_packs_minecraft_version() {
        let server = MockServer::start().await;
        let stale_file = REQUIRED_FILE;
        let current_file = REQUIRED_FILE + 1;
        let manifest = serve_and_declare_the_jei_duplicate(&server, stale_file, current_file).await;

        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_with_manifest(dir.path(), "repro.zip", &manifest);
        let app = app_against(dir.path(), &server);

        let imported = import(&app, &archive)
            .await
            .expect("a duplicate declaration does not fail the import");

        assert!(
            imported
                .materialized
                .files
                .installed
                .iter()
                .any(|file| file.ends_with("jei-1.16.5-7.7.0.101.jar")),
            "the file matching the pack's own Minecraft version is installed"
        );
        assert!(
            imported
                .materialized
                .files
                .installed
                .iter()
                .all(|file| !file.ends_with("jei-1.16.4-7.6.1.65.jar")),
            "the stale, non-matching file is never installed"
        );
        assert!(
            imported.record.attention.is_empty(),
            "a duplicate declaration is not carried onto the instance"
        );
    }

    #[tokio::test]
    async fn a_project_declared_twice_with_no_matching_version_installs_the_newer_file() {
        // Neither candidate names the pack's Minecraft version, so the tie
        // falls to whichever CurseForge published more recently.
        let server = MockServer::start().await;
        serve_pack_as_mods(&server).await;

        let older_file = REQUIRED_FILE;
        let newer_file = REQUIRED_FILE + 1;
        serve_file_with_game_versions_and_date(
            &server,
            REQUIRED_PROJECT,
            older_file,
            "older.jar",
            REQUIRED_BODY,
            &["1.19.2", "Forge"],
            "2022-01-01T00:00:00.000Z",
        )
        .await;
        serve_file_with_game_versions_and_date(
            &server,
            REQUIRED_PROJECT,
            newer_file,
            "newer.jar",
            REQUIRED_BODY,
            &["1.19.4", "Forge"],
            "2023-06-01T00:00:00.000Z",
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

        let manifest = export_manifest_with_files(&format!(
            r#"{{"projectID": {REQUIRED_PROJECT}, "fileID": {older_file}, "required": true}},
               {{"projectID": {REQUIRED_PROJECT}, "fileID": {newer_file}, "required": true}},
               {{"projectID": {OPTIONAL_PROJECT}, "fileID": {OPTIONAL_FILE}, "required": false}}"#
        ));
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_with_manifest(dir.path(), "repro.zip", &manifest);
        let app = app_against(dir.path(), &server);

        let mut analysis = app.analyze_pack(&archive).expect("analyzed locally");
        app.resolve_pack(&mut analysis, &[], &CancellationToken::new())
            .await
            .expect("a duplicate declaration does not fail resolution");
        assert_eq!(
            analysis.duplicate_files[0].reason,
            DuplicateFileReason::MoreRecentlyPublished,
            "neither file matched the pack's Minecraft version"
        );

        let imported = import(&app, &archive)
            .await
            .expect("a duplicate declaration does not fail the import");

        assert!(
            imported
                .materialized
                .files
                .installed
                .iter()
                .any(|file| file.ends_with("newer.jar")),
            "the more recently published file wins when neither matches"
        );
        assert!(
            imported
                .materialized
                .files
                .installed
                .iter()
                .all(|file| !file.ends_with("older.jar")),
        );
    }

    /// Serves the required project's two declared files, each either
    /// restricted or downloadable, and returns the manifest declaring both.
    /// The newer file is the one the duplicate rule keeps.
    async fn serve_a_duplicate_declaration_with_restrictions(
        server: &MockServer,
        older_restricted: bool,
        newer_restricted: bool,
    ) -> Vec<u8> {
        serve_pack_as_mods(server).await;
        for (file, name, restricted, date) in [
            (
                REQUIRED_FILE,
                "older.jar",
                older_restricted,
                "2022-01-01T00:00:00.000Z",
            ),
            (
                REQUIRED_FILE + 1,
                "newer.jar",
                newer_restricted,
                "2023-06-01T00:00:00.000Z",
            ),
        ] {
            if restricted {
                serve_restricted_file_with_game_versions_and_date(
                    server,
                    REQUIRED_PROJECT,
                    file,
                    name,
                    REQUIRED_BODY,
                    &["1.19.2", "Forge"],
                    date,
                )
                .await;
            } else {
                serve_file_with_game_versions_and_date(
                    server,
                    REQUIRED_PROJECT,
                    file,
                    name,
                    REQUIRED_BODY,
                    &["1.19.2", "Forge"],
                    date,
                )
                .await;
            }
        }
        serve_file(
            server,
            OPTIONAL_PROJECT,
            OPTIONAL_FILE,
            "optional-1.0.0.jar",
            OPTIONAL_BODY,
        )
        .await;
        export_manifest_with_files(&format!(
            r#"{{"projectID": {REQUIRED_PROJECT}, "fileID": {older}, "required": true}},
               {{"projectID": {REQUIRED_PROJECT}, "fileID": {newer}, "required": true}},
               {{"projectID": {OPTIONAL_PROJECT}, "fileID": {OPTIONAL_FILE}, "required": false}}"#,
            older = REQUIRED_FILE,
            newer = REQUIRED_FILE + 1,
        ))
    }

    #[tokio::test]
    async fn a_restricted_duplicate_of_a_downloadable_winner_is_discarded_without_a_manual_download()
     {
        let server = MockServer::start().await;
        let manifest = serve_a_duplicate_declaration_with_restrictions(&server, true, false).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_with_manifest(dir.path(), "repro.zip", &manifest);
        let app = app_against(dir.path(), &server);

        let mut analysis = app.analyze_pack(&archive).expect("analyzed locally");
        app.resolve_pack(&mut analysis, &[], &CancellationToken::new())
            .await
            .expect("a duplicate declaration does not fail resolution");

        assert!(
            analysis.blocked.is_empty(),
            "the discarded restricted copy is never requested by hand"
        );
        assert_eq!(analysis.duplicate_files.len(), 1);
        assert_eq!(
            analysis.duplicate_files[0].discarded_file_name.as_deref(),
            Some("older.jar")
        );
        assert_eq!(
            analysis.duplicate_files[0].kept_file_name.as_deref(),
            Some("newer.jar")
        );
        let required_entries = analysis
            .recipe
            .files
            .iter()
            .filter(|file| {
                file.logical_path.ends_with("newer.jar") || file.logical_path.ends_with("older.jar")
            })
            .count();
        assert_eq!(required_entries, 1, "exactly one file per project survives");
    }

    #[tokio::test]
    async fn a_cached_restricted_duplicate_of_a_downloadable_winner_is_not_installed_beside_it() {
        let server = MockServer::start().await;
        let manifest = serve_a_duplicate_declaration_with_restrictions(&server, true, false).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_with_manifest(dir.path(), "repro.zip", &manifest);
        let app = app_against(dir.path(), &server);

        // A verified copy of the discarded restricted file already sits in the
        // shared cache, as if an earlier import had put it there.
        let hash = Hash::new(HashAlgorithm::Sha1, &sha1_of(REQUIRED_BODY)).expect("valid sha1");
        let extension = FileExtension::new("jar").expect("jar");
        let cache_path =
            app.artifact_cache()
                .layout()
                .content(ContentStore::Downloads, &hash, Some(&extension));
        std::fs::create_dir_all(cache_path.parent().expect("shard")).expect("shard");
        std::fs::write(&cache_path, REQUIRED_BODY).expect("pre-populate the cache");

        let imported = import(&app, &archive)
            .await
            .expect("a duplicate declaration does not fail the import");

        let installed: Vec<_> = imported
            .materialized
            .files
            .installed
            .iter()
            .filter(|file| file.ends_with("newer.jar") || file.ends_with("older.jar"))
            .collect();
        assert_eq!(
            installed.len(),
            1,
            "two jars of one project would be refused by the game"
        );
        assert!(installed[0].ends_with("newer.jar"));
        assert!(imported.record.attention.is_empty());
    }

    #[tokio::test]
    async fn a_restricted_winner_over_a_downloadable_duplicate_is_the_only_blocked_file() {
        let server = MockServer::start().await;
        let manifest = serve_a_duplicate_declaration_with_restrictions(&server, false, true).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_with_manifest(dir.path(), "repro.zip", &manifest);
        let app = app_against(dir.path(), &server);

        let mut analysis = app.analyze_pack(&archive).expect("analyzed locally");
        app.resolve_pack(&mut analysis, &[], &CancellationToken::new())
            .await
            .expect("a duplicate declaration does not fail resolution");

        assert_eq!(analysis.blocked.len(), 1);
        assert_eq!(analysis.blocked[0].file_name.as_deref(), Some("newer.jar"));
        assert_eq!(analysis.duplicate_files.len(), 1);
        assert_eq!(
            analysis.duplicate_files[0].discarded_file_name.as_deref(),
            Some("older.jar")
        );
        assert!(
            analysis
                .recipe
                .files
                .iter()
                .all(|file| !file.logical_path.ends_with("older.jar")),
            "the downloadable loser is not installed beside the blocked winner"
        );
    }

    #[tokio::test]
    async fn two_restricted_duplicates_of_one_project_block_only_the_kept_file() {
        let server = MockServer::start().await;
        let manifest = serve_a_duplicate_declaration_with_restrictions(&server, true, true).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_with_manifest(dir.path(), "repro.zip", &manifest);
        let app = app_against(dir.path(), &server);

        let mut analysis = app.analyze_pack(&archive).expect("analyzed locally");
        app.resolve_pack(&mut analysis, &[], &CancellationToken::new())
            .await
            .expect("a duplicate declaration does not fail resolution");

        assert_eq!(analysis.blocked.len(), 1, "one manual download, not two");
        assert_eq!(analysis.blocked[0].file_name.as_deref(), Some("newer.jar"));
        assert_eq!(analysis.duplicate_files.len(), 1);
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
        // A CurseForge manifest declares no server side for anything, so under
        // the default policy the whole pack is what a linked server reproduces.
        let plan = recipe
            .server_reconstruction_plan(&UnknownServerFiles::default())
            .expect("a plan");
        assert_eq!(plan.files.len(), 2);
        assert!(plan.unconfirmed_unknown.is_empty());
        assert_eq!(plan.minecraft_version, "1.20.1");
        assert_eq!(plan.loader.version.as_deref(), Some("47.4.10"));
        assert!(
            plan.layers_are_retained(),
            "the pack's overrides were kept for a server built later"
        );
        assert!(
            plan.files
                .iter()
                .all(|file| file.origin == ModOrigin::PackDeclared)
        );

        // Naming paths narrows it, and the exact identity and ownership survive
        // that too.
        let confirmed = recipe
            .server_reconstruction_plan(&UnknownServerFiles::only(OptionalSelection::of([
                "mods/required-1.0.0.jar",
            ])))
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
        assert_eq!(confirmed.unconfirmed_unknown.len(), 1);
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
        // The pack declares nothing about servers, so the file this suite's
        // fixture cares about is named explicitly rather than left to the
        // default policy. These tests are about what reconstruction does with
        // one known file, not about which files it picks.
        reconstruct_server_confirming(
            app,
            imported,
            staging,
            UnknownServerFiles::only(OptionalSelection::of(["mods/required-1.0.0.jar"])),
        )
        .await
    }

    /// The same reconstruction under whichever undeclared-server policy the
    /// caller wants, instead of always [`reconstruct_server`]'s one required
    /// mod.
    async fn reconstruct_server_confirming(
        app: &App,
        imported: &crate::import::ImportedPack,
        staging: &Path,
        unknown_server_files: UnknownServerFiles,
    ) -> CoreResult<PathBuf> {
        let recipe = app
            .pack_recipe(imported.record.id)
            .expect("query")
            .expect("an imported instance has a recipe");

        let target = staging.join("minecraft");
        let options = ServerCreateOptions {
            java_path: None,
            install_java_if_missing: false,
            unknown_server_files,
            content: ServerContentSource::Recipe,
            reporter: None,
        };
        app.materialize_linked_server_recipe(
            &recipe,
            &imported.record.root,
            &target,
            &staging.join(".launcher/source"),
            None,
            &options,
            &CancellationToken::new(),
        )
        .await?;
        Ok(target)
    }

    #[tokio::test]
    async fn a_linked_server_missing_a_restricted_release_requests_manual_acquisition() {
        let server = MockServer::start().await;
        serve_pack_as_mods(&server).await;
        let dir = tempfile::tempdir().expect("temp dir");
        let app = app_against(dir.path(), &server);
        let mut recipe = ephervex_pack::PackRecipe::for_manual_instance(
            "Repro",
            "1.20.1",
            ephervex_model::LoaderSelection::vanilla(),
        );
        recipe.files.push(PackFileRecipe {
            logical_path: PathBuf::from("mods/restricted.jar"),
            hashes: vec![
                Hash::new(HashAlgorithm::Sha1, &sha1_of(b"restricted body")).expect("digest"),
            ],
            downloads: Vec::new(),
            size: Some(15),
            client: SideRequirement::Required,
            server: SideRequirement::Required,
            selected: None,
            origin: ModOrigin::UserSelected,
            identity: Some(ProviderFileIdentity {
                provider: Some(ModProvider::CurseForge),
                project_id: Some(REQUIRED_PROJECT.to_string()),
                version_id: Some(REQUIRED_FILE.to_string()),
                file_id: Some(REQUIRED_FILE.to_string()),
            }),
            original_identity: None,
            relations: Some(Vec::new()),
        });
        let staging = dir.path().join("linked-server");

        let error = app
            .materialize_linked_server_recipe(
                &recipe,
                &dir.path().join("client"),
                &staging.join("minecraft"),
                &staging.join(".launcher/source"),
                None,
                &ServerCreateOptions::default(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("the user has to supply the restricted release");

        let CoreError::ModAcquisitionRestricted { path, blocked } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert_eq!(path, PathBuf::from("mods/restricted.jar"));
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].project_name.as_deref(), Some("Required Mod"));
        assert_eq!(blocked[0].file_name.as_deref(), Some("restricted.jar"));
        assert_eq!(
            blocked[0].browser_url.as_deref(),
            Some("https://www.curseforge.com/minecraft/mc-mods/Required Mod/download/4593548")
        );
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
            original_identity: None,
            relations: None,
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
        // allowlist does not contain but Modrinth's redirect-only hosts do.
        // Following it is correct - this request carries no CurseForge secret -
        // and proves that one provider's redirect containment did not become
        // everybody's.
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
            "an unauthenticated download still follows a redirect its own hosts allow"
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
    async fn a_managed_modrinth_artifact_naming_an_unapproved_host_is_never_requested() {
        let server = MockServer::start().await;
        serve_pack_guarded(&server).await;
        Mock::given(method("GET"))
            .and(path("/modrinth/from-modrinth.jar"))
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
        // A provider response named an address on a host that is not Modrinth's:
        // `localhost` reaches the same loopback server but is not on the list an
        // address may name.
        let mut entry = modrinth_entry(&server);
        entry.downloads = vec![format!(
            "http://localhost:{}/modrinth/from-modrinth.jar",
            server.address().port()
        )];
        let source = SourceStore::new(app.pack_source_dir(imported.record.id).expect("source dir"));
        let mut recipe = source.read_recipe().expect("the retained recipe");
        recipe.files.push(entry);
        source
            .write_recipe(&recipe)
            .expect("retain the mixed recipe");

        let error = reconstruct_server(&app, &imported, &dir.path().join("server"))
            .await
            .expect_err("the initial address is off the approved hosts");

        assert!(
            matches!(
                &error,
                CoreError::Pack(PackError::DownloadFailed { source, .. })
                    if matches!(
                        **source,
                        ephervex_download::DownloadError::Policy(
                            ephervex_download::UrlPolicyError::HostNotAllowed { ref host, .. }
                        ) if host == "localhost"
                    )
            ),
            "{error:?}"
        );
        let (requested, _) = artifact_requests(&server, "/modrinth/").await;
        assert_eq!(requested, 0, "the unapproved host was contacted");
    }

    #[tokio::test]
    async fn a_modrinth_artifact_is_not_redirected_off_its_permitted_hosts() {
        let server = MockServer::start().await;
        serve_pack_guarded(&server).await;
        // A loopback address no Modrinth host list names, so the redirect
        // policy alone decides whether it is contacted.
        let elsewhere = format!(
            "http://127.0.0.2:{}/relocated/from-modrinth.jar",
            server.address().port()
        );
        Mock::given(method("GET"))
            .and(path("/modrinth/from-modrinth.jar"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", elsewhere.as_str()))
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
            .expect_err("the redirect leaves Modrinth's hosts");
        assert!(
            matches!(
                &error,
                CoreError::Pack(PackError::DownloadFailed { source, .. })
                    if matches!(**source, ephervex_download::DownloadError::RedirectRefused { ref host, .. } if host == "127.0.0.2")
            ),
            "{error:?}"
        );
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
        // the key is never read by this test - only by the transport, which
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
