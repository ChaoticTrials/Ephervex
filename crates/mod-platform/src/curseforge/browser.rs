//! The CurseForge mod catalog, as a provider the browser can use.
//!
//! One implementation of [`ModProviderApi`], built on the same authenticated
//! transport a pack import uses. Nothing here is a second browser: the
//! resolver, the plan, the compatibility rules, and both frontends are the
//! ones that were already there, and this module only answers their questions
//! in CurseForge's terms.
//!
//! # Every answer is scoped to one instance
//!
//! CurseForge's file listings, and its searches for compatible mods, are
//! filtered server-side by the exact Minecraft release and the loader family.
//! That is not an optimization: the file listing is paged, and a project with a
//! decade of releases cannot be enumerated to answer "what fits this instance".
//! The exact release is passed through untouched and the loader is never
//! crossed. A search that asks for incompatible results leaves both filters
//! off, and each row is still classified from its own narrowed file listing.
//!
//! # What the class filter is, and what it is not
//!
//! Searches carry the provider-discovered Mods class, so resource packs,
//! worlds, shader packs, and modpacks are not in the answer. That filter is not
//! taken as proof: every project document that comes back is classified again
//! against the classes this same operation asked for, and one that cannot be
//! confirmed as a Mod is refused or left out rather than installed as one.
//!
//! # Nothing is written down
//!
//! Each method opens its own [`CurseForgeOperation`], which reads the
//! credential once and drops it when the method returns. No response is
//! cached, staged, or persisted; searching twice asks CurseForge twice. See
//! [`super`] for why that is a requirement rather than an omission.

use async_trait::async_trait;
use ephervex_download::{DomainAllowlist, OperationReporter};
use ephervex_model::{CurseForgeFileId, CurseForgeProjectId, LoaderKind, ModProvider};
use futures_util::StreamExt as _;
use tokio_util::sync::CancellationToken;
use tracing::{instrument, warn};

use super::api::{
    CurseForgeApi, CurseForgeOperation, CurseForgeProjectClasses, CurseForgeSearch,
    MINECRAFT_GAME_ID, loader_name,
};
use super::mapping;
use crate::error::{ModPlatformError, ModPlatformResult};
use crate::model::{
    Compatibility, IncompatibilityReason, InstanceTarget, ModProject, ModSearchHit, ModSearchQuery,
    ModSearchResults, ModVersionInfo, ProjectBatch,
};
use crate::provider::ModProviderApi;
use crate::resolve::newest_compatible;

/// How many projects' file listings run at once while annotating a page.
///
/// Compatibility cannot be read off a search response, so each hit costs one
/// file-listing request. Six keeps a page responsive without turning one search
/// into a burst against a rate limit the user's own key is charged for.
const MAX_CONCURRENT_PROJECT_LOOKUPS: usize = 6;

/// Reads CurseForge's project and file catalog.
#[derive(Debug, Clone)]
pub struct CurseForgeProvider {
    api: CurseForgeApi,
    hosts: DomainAllowlist,
}

impl CurseForgeProvider {
    /// A provider over an API client, accepting artifact addresses from the
    /// hosts CurseForge delivers its own files from.
    #[must_use]
    pub fn new(api: CurseForgeApi) -> Self {
        Self::with_artifact_hosts(api, DomainAllowlist::curseforge_artifact_defaults())
    }

    /// The same, with an explicit list of hosts an artifact address may name.
    ///
    /// The list is a parameter for the same reason the API root is: an
    /// integration test points both at a loopback server. Production callers
    /// use [`Self::new`], and a release build has no way to widen the list
    /// except by saying so at the call site.
    #[must_use]
    pub fn with_artifact_hosts(api: CurseForgeApi, hosts: DomainAllowlist) -> Self {
        Self { api, hosts }
    }

    /// The project id a human-typable reference names.
    ///
    /// A numeric id is used as it stands. Anything else is looked up as a slug
    /// through the documented search endpoint, because CurseForge addresses
    /// projects by number and has no by-slug route.
    async fn project_id(
        &self,
        operation: &CurseForgeOperation<'_>,
        classes: &CurseForgeProjectClasses,
        reference: &str,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<CurseForgeProjectId> {
        let reference = reference.trim();
        if let Some(id) = CurseForgeProjectId::parse(reference) {
            return Ok(id);
        }
        operation
            .project_by_slug(
                MINECRAFT_GAME_ID,
                classes.mods_class_id(),
                reference,
                cancel,
            )
            .await?
            .and_then(|project| CurseForgeProjectId::new(project.id))
            .ok_or_else(|| ModPlatformError::ProjectUnknown {
                provider: ModProvider::CurseForge,
                reference: reference.to_owned(),
            })
    }

    /// One project's compatible files, already narrowed to the target and
    /// limited to files that project owns.
    async fn compatible_files(
        &self,
        operation: &CurseForgeOperation<'_>,
        project: CurseForgeProjectId,
        target: &InstanceTarget,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<ModVersionInfo>> {
        let owner = project.to_string();
        let mut versions: Vec<ModVersionInfo> = operation
            .project_files(project, &target.minecraft_version, target.loader, cancel)
            .await?
            .into_iter()
            .map(|document| mapping::version(document, &self.hosts))
            .collect::<ModPlatformResult<_>>()?;
        versions.retain(|version| version.project_id == owner);
        Ok(versions)
    }
}

/// Why a search row has no file for the instance, read from the releases and
/// loaders its latest files declare.
///
/// The row's own file listing was filtered to the instance, so it cannot say
/// which part failed. A part the declarations do not list is reported; when
/// they list both, no single file fits both and the loader is named, as the
/// narrower claim. A project that declares nothing is reported against the
/// release with nothing to list.
fn search_incompatibility(
    target: &InstanceTarget,
    loader: &str,
    declared: &(Vec<String>, Vec<String>),
) -> Compatibility {
    let (versions, loaders) = declared;
    if versions.is_empty() && loaders.is_empty() {
        return Compatibility::Incompatible(vec![IncompatibilityReason::MinecraftVersion {
            required: target.minecraft_version.clone(),
            supported: Vec::new(),
        }]);
    }
    let mut reasons = Vec::new();
    if !versions.contains(&target.minecraft_version) {
        reasons.push(IncompatibilityReason::MinecraftVersion {
            required: target.minecraft_version.clone(),
            supported: versions.clone(),
        });
    }
    if !loaders.iter().any(|name| name.eq_ignore_ascii_case(loader)) {
        reasons.push(IncompatibilityReason::Loader {
            required: target.loader,
            supported: loaders.clone(),
        });
    }
    if reasons.is_empty() {
        reasons.push(IncompatibilityReason::Loader {
            required: target.loader,
            supported: Vec::new(),
        });
    }
    Compatibility::Incompatible(reasons)
}

/// The project id a neutral reference carries, or a refusal naming it.
fn project_id_of(reference: &str) -> ModPlatformResult<CurseForgeProjectId> {
    CurseForgeProjectId::parse(reference.trim()).ok_or_else(|| ModPlatformError::ProjectUnknown {
        provider: ModProvider::CurseForge,
        reference: reference.to_owned(),
    })
}

#[async_trait]
impl ModProviderApi for CurseForgeProvider {
    fn provider(&self) -> ModProvider {
        ModProvider::CurseForge
    }

    fn loader_name(&self, loader: LoaderKind) -> Option<&'static str> {
        loader_name(loader)
    }

    #[instrument(skip_all, fields(query = %query.text, incompatible = query.include_incompatible))]
    async fn search(
        &self,
        target: &InstanceTarget,
        query: &ModSearchQuery,
        _reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<ModSearchResults> {
        // Refused here rather than after a request: an instance with no loader
        // cannot host mods, and there is no filter that would make the answer
        // mean anything.
        crate::provider::loader_name_for(self, target)?;

        let operation = self.api.begin_operation()?;
        // Discovered for this operation. Which number means "Mods" is provider
        // metadata, not something this launcher is allowed to assume.
        let classes = operation.project_classes(MINECRAFT_GAME_ID, cancel).await?;

        let page = operation
            .search(
                MINECRAFT_GAME_ID,
                &CurseForgeSearch {
                    text: &query.text,
                    minecraft_version: &target.minecraft_version,
                    loader: target.loader,
                    include_incompatible: query.include_incompatible,
                    class_id: classes.mods_class_id(),
                    index: query.offset,
                    page_size: u32::from(query.limit),
                },
                cancel,
            )
            .await?;
        let total_hits = page.total;
        let next_offset = page.next_offset;

        // The class filter narrowed the request; this decides what the answer
        // actually is. A project whose class cannot be confirmed is left out
        // rather than shown as a mod.
        let mut projects = Vec::new();
        for raw in page.items {
            let id = raw.id;
            let declared = mapping::project_declared_support(&raw);
            match mapping::checked_project(raw, &classes) {
                Ok(project) => projects.push((project, declared)),
                Err(error) => {
                    warn!(
                        project = id,
                        %error,
                        "leaving a search result out: it could not be confirmed to be a mod"
                    );
                }
            }
        }

        // Each hit needs its own file listing: a search response cannot show
        // that one file supports both this Minecraft release and this loader.
        // Bounded so a page of results is not a burst of requests.
        let annotated =
            futures_util::stream::iter(projects.into_iter().map(|(project, declared)| {
                let operation = &operation;
                async move {
                    let versions = match project_id_of(&project.id) {
                        Ok(id) => self.compatible_files(operation, id, target, cancel).await,
                        Err(error) => Err(error),
                    };
                    (project, declared, versions)
                }
            }))
            .buffered(MAX_CONCURRENT_PROJECT_LOOKUPS)
            .collect::<Vec<_>>()
            .await;

        let loader = crate::provider::loader_name_for(self, target)?;
        let mut hits = Vec::new();
        for (project, declared, versions) in annotated {
            if cancel.is_cancelled() {
                return Err(ModPlatformError::Canceled);
            }
            let versions = match versions {
                Ok(versions) => versions,
                Err(error) => {
                    // One unreadable project is not a reason to fail the whole
                    // search; it is a reason to leave that row out and say so
                    // in the log. A project whose author turned off third-party
                    // downloads arrives here, which is why it is not offered.
                    warn!(project = %project.id, %error, "skipping a search result whose files could not be read");
                    continue;
                }
            };
            let best = newest_compatible(&versions, target, loader);
            let compatibility = match &best {
                Some(_) => Compatibility::Compatible,
                None => search_incompatibility(target, loader, &declared),
            };
            if !query.include_incompatible && !compatibility.is_compatible() {
                continue;
            }
            hits.push(ModSearchHit {
                project,
                best_version: best,
                compatibility,
            });
        }

        Ok(ModSearchResults {
            provider: ModProvider::CurseForge,
            hits,
            total_hits,
            offset: query.offset,
            next_offset,
        })
    }

    #[instrument(skip_all, fields(project = reference))]
    async fn project(
        &self,
        reference: &str,
        _reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<ModProject> {
        let operation = self.api.begin_operation()?;
        let classes = operation.project_classes(MINECRAFT_GAME_ID, cancel).await?;
        let id = self
            .project_id(&operation, &classes, reference, cancel)
            .await?;
        let raw = operation.project(id, cancel).await?;
        mapping::checked_project(raw, &classes)
    }

    #[instrument(skip_all, fields(project = project_id))]
    async fn versions(
        &self,
        project_id: &str,
        target: &InstanceTarget,
        _reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<ModVersionInfo>> {
        let operation = self.api.begin_operation()?;
        self.compatible_files(&operation, project_id_of(project_id)?, target, cancel)
            .await
    }

    async fn project_batch_by_id(
        &self,
        ids: &[String],
        _reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<ProjectBatch> {
        let mut batch = ProjectBatch::default();
        if ids.is_empty() {
            return Ok(batch);
        }
        let projects: Vec<CurseForgeProjectId> = ids
            .iter()
            .map(|id| project_id_of(id))
            .collect::<ModPlatformResult<_>>()?;

        let operation = self.api.begin_operation()?;
        let classes = operation.project_classes(MINECRAFT_GAME_ID, cancel).await?;
        for raw in operation.projects(&projects, cancel).await? {
            mapping::sort_project(&mut batch, raw, &classes)?;
        }
        Ok(batch)
    }

    async fn versions_by_id(
        &self,
        ids: &[String],
        _reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<ModVersionInfo>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let files: Vec<CurseForgeFileId> = ids
            .iter()
            .map(|id| {
                CurseForgeFileId::parse(id.trim()).ok_or_else(|| ModPlatformError::VersionUnknown {
                    provider: ModProvider::CurseForge,
                    version_id: id.clone(),
                })
            })
            .collect::<ModPlatformResult<_>>()?;

        let operation = self.api.begin_operation()?;
        operation
            .files(&files, cancel)
            .await?
            .into_iter()
            .map(|document| mapping::version(document, &self.hosts))
            .collect()
    }
}
