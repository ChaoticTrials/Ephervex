//! The Modrinth provider.
//!
//! Modrinth's public API documentation is authoritative here; this module and
//! its submodules are a reader for it and nothing else. Everything they learn
//! becomes [`crate::model`] types before it leaves, so nothing above this file
//! sees a facet string or a `dependency_type`.
//!
//! ```text
//! modrinth.rs     the provider: what to ask for, and what to do with the answer
//! ├── api         endpoints, requests, and the metadata cache
//! ├── cache_key   what identifies one cached answer
//! ├── dto         Modrinth's wire shapes, declared and nothing more
//! └── mapping     what those shapes mean as Ephervex's own model
//! ```
//!
//! The split is along the question each file answers. `api` knows how to ask
//! Modrinth something; `dto` knows what comes back; `mapping` knows how to read
//! it; and this file knows which of those to do in what order - including the
//! one thing none of them can decide alone, that compatibility comes from a
//! concrete version rather than from anything a search response says.

mod api;
mod cache_key;
mod dto;
mod mapping;

use std::collections::BTreeMap;

use async_trait::async_trait;
use ephervex_download::{CacheLayout, DownloadManager, OperationReporter};
use ephervex_model::{Hash, LoaderKind, ModProvider};
use futures_util::StreamExt as _;
use tokio_util::sync::CancellationToken;
use tracing::{instrument, warn};

use crate::error::{ModPlatformError, ModPlatformResult};
use crate::model::{
    Compatibility, IncompatibilityReason, InstanceTarget, ModProject, ModSearchHit, ModSearchQuery,
    ModSearchResults, ModVersionInfo, RecognizedFile,
};
use crate::provider::ModProviderApi;
use crate::resolve::newest_compatible;
use api::ModrinthApi;

pub use api::MODRINTH_API_BASE;

/// The project type this launcher browses.
///
/// A modpack is imported, not installed into an instance, and a resource pack
/// is not a mod. Filtering upstream keeps both out of the list rather than
/// offering something that cannot be installed.
const PROJECT_TYPE: &str = "mod";

/// How many project lookups run at once while annotating a page of results.
///
/// Compatibility cannot be read off a search response, so each hit costs one
/// version-list request. Six keeps a page responsive without turning one search
/// into a burst against a shared rate limit.
const MAX_CONCURRENT_PROJECT_LOOKUPS: usize = 6;

/// Modrinth's own names for the loaders this launcher supports.
const LOADER_NAMES: &[(LoaderKind, &str)] = &[
    (LoaderKind::Forge, "forge"),
    (LoaderKind::NeoForge, "neoforge"),
];

/// Reads Modrinth's project and version catalog.
#[derive(Debug, Clone)]
pub struct ModrinthProvider {
    api: ModrinthApi,
}

impl ModrinthProvider {
    /// A provider against the production API.
    pub fn new(manager: DownloadManager, layout: CacheLayout) -> ModPlatformResult<Self> {
        Self::with_base_url(manager, layout, MODRINTH_API_BASE)
    }

    /// A provider against an explicit API root.
    ///
    /// Exists so integration tests can point the whole provider at a loopback
    /// server, the same way `ephervex-loaders` does for its metadata sources.
    /// Production always uses [`MODRINTH_API_BASE`], and the download manager's
    /// transport policy still refuses plaintext HTTP unless a test has
    /// explicitly opted in.
    pub fn with_base_url(
        manager: DownloadManager,
        layout: CacheLayout,
        base: &str,
    ) -> ModPlatformResult<Self> {
        Ok(Self {
            api: ModrinthApi::new(manager, layout, base)?,
        })
    }

    /// Which Modrinth releases these files on disk are.
    ///
    /// Answers only about digests it was asked with, and says nothing about the
    /// ones it does not recognize: a file Modrinth has never seen is simply
    /// absent from the result, which is not a failure.
    pub async fn recognize(
        &self,
        digests: &[Hash],
        cancel: &CancellationToken,
    ) -> ModPlatformResult<BTreeMap<String, RecognizedFile>> {
        self.api.version_files(digests, cancel).await
    }
}

#[async_trait]
impl ModProviderApi for ModrinthProvider {
    fn provider(&self) -> ModProvider {
        ModProvider::Modrinth
    }

    fn loader_name(&self, loader: LoaderKind) -> Option<&'static str> {
        LOADER_NAMES
            .iter()
            .find(|(kind, _)| *kind == loader)
            .map(|(_, name)| *name)
    }

    #[instrument(skip_all, fields(query = %query.text, incompatible = query.include_incompatible))]
    async fn search(
        &self,
        target: &InstanceTarget,
        query: &ModSearchQuery,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<ModSearchResults> {
        let loader = crate::provider::loader_name_for(self, target)?;
        let response = self
            .api
            .search(target, query, loader, reporter, cancel)
            .await?;
        // Read before `response.hits` is consumed below: this is the raw page
        // the provider actually returned, which is what the next offset has
        // to continue from, not how many of those hits survive the
        // compatibility check that follows.
        let next_offset = crate::model::next_search_offset(
            query.offset,
            response.hits.len() as u64,
            response.total_hits,
        );

        // Each hit needs its own version list: neither the facets nor the hit's
        // project-wide arrays can show that one version supports both this
        // Minecraft release and this loader. Bounded so a page of results is
        // not a burst of requests.
        let annotated =
            futures_util::stream::iter(response.hits.into_iter().map(|hit| async move {
                let project = mapping::hit_project(hit);
                let versions = self.versions(&project.id, target, reporter, cancel).await;
                (project, versions)
            }))
            .buffered(MAX_CONCURRENT_PROJECT_LOOKUPS)
            .collect::<Vec<_>>()
            .await;

        let mut hits = Vec::new();
        for (project, versions) in annotated {
            if cancel.is_cancelled() {
                return Err(ModPlatformError::Canceled);
            }
            let versions = match versions {
                Ok(versions) => versions,
                Err(error) => {
                    // One unreadable project is not a reason to fail the whole
                    // search; it is a reason to leave that row out and say so
                    // in the log.
                    warn!(project = %project.id, %error, "skipping a search result whose versions could not be read");
                    continue;
                }
            };
            let best = newest_compatible(&versions, target, loader);
            let compatibility = match &best {
                Some(_) => Compatibility::Compatible,
                None => project_incompatibility(&versions, target, loader),
            };
            if !query.include_incompatible && !compatibility.is_compatible() {
                // The facets promised more than they can deliver.
                continue;
            }
            hits.push(ModSearchHit {
                project,
                best_version: best,
                compatibility,
            });
        }

        Ok(ModSearchResults {
            provider: ModProvider::Modrinth,
            hits,
            total_hits: response.total_hits,
            offset: query.offset,
            next_offset,
        })
    }

    #[instrument(skip_all, fields(project = reference))]
    async fn project(
        &self,
        reference: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<ModProject> {
        let raw = self.api.project(reference, reporter, cancel).await?;
        mapping::checked_project(raw, reference)
    }

    #[instrument(skip_all, fields(project = project_id))]
    async fn versions(
        &self,
        project_id: &str,
        // Modrinth serves a project's whole version list as one cacheable
        // document, so the answer does not depend on the instance: the local
        // compatibility check narrows it, and keeping the wide list is what
        // lets an incompatible version be shown with the reason it does not
        // fit.
        _target: &InstanceTarget,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<ModVersionInfo>> {
        let raw = self.api.versions(project_id, reporter, cancel).await?;
        raw.into_iter().map(mapping::version).collect()
    }

    async fn projects_by_id(
        &self,
        ids: &[String],
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<ModProject>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let raw = self.api.projects_by_id(ids, reporter, cancel).await?;
        Ok(raw.into_iter().map(mapping::project).collect())
    }

    async fn versions_by_id(
        &self,
        ids: &[String],
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<ModVersionInfo>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let raw = self.api.versions_by_id(ids, reporter, cancel).await?;
        raw.into_iter().map(mapping::version).collect()
    }
}

/// Why nothing in a project fits, summarized from its versions.
///
/// Reported per project rather than per version so a search row can say "this
/// one is for 1.21.1" instead of listing forty releases.
fn project_incompatibility(
    versions: &[ModVersionInfo],
    target: &InstanceTarget,
    loader: &str,
) -> Compatibility {
    let mut supported_versions: Vec<String> = Vec::new();
    let mut supported_loaders: Vec<String> = Vec::new();
    for version in versions {
        for game_version in &version.game_versions {
            if !supported_versions.contains(game_version) {
                supported_versions.push(game_version.clone());
            }
        }
        for name in &version.loaders {
            if !supported_loaders.contains(name) {
                supported_loaders.push(name.clone());
            }
        }
    }

    let mut reasons = Vec::new();
    if !supported_versions.contains(&target.minecraft_version) {
        reasons.push(IncompatibilityReason::MinecraftVersion {
            required: target.minecraft_version.clone(),
            supported: supported_versions,
        });
    }
    if !supported_loaders
        .iter()
        .any(|name| name.eq_ignore_ascii_case(loader))
    {
        reasons.push(IncompatibilityReason::Loader {
            required: target.loader,
            supported: supported_loaders,
        });
    }
    if reasons.is_empty() {
        // Every part is supported by *something*, but no single version
        // supports both. That is exactly the case facets get wrong, and it
        // deserves the reason that is actually true.
        reasons.push(IncompatibilityReason::Loader {
            required: target.loader,
            supported: Vec::new(),
        });
    }
    Compatibility::Incompatible(reasons)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn provider() -> ModrinthProvider {
        ModrinthProvider::new(
            DownloadManager::new(ephervex_download::DownloaderConfig::default()).expect("manager"),
            CacheLayout::new("/tmp/ephervex-mod-platform-test"),
        )
        .expect("provider")
    }

    #[test]
    fn vanilla_instances_have_no_loader_to_browse_for() {
        let target = InstanceTarget::new("1.20.1", LoaderKind::Vanilla);
        let error = crate::provider::loader_name_for(&provider(), &target)
            .expect_err("vanilla cannot host mods");
        assert!(matches!(
            error,
            ModPlatformError::LoaderCannotHostMods {
                loader: LoaderKind::Vanilla
            }
        ));
    }
}
