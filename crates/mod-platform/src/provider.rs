//! The provider boundary (ADR 0005).
//!
//! Four methods, each one something the mod browser actually needs. The trait
//! is deliberately not shaped around Modrinth's endpoint list: a provider that
//! has no notion of a version list, or that needs two calls to produce one,
//! implements these by doing that.
//!
//! # What the trait does *not* do
//!
//! It does not decide compatibility, choose a default version, or resolve a
//! dependency graph. Those are the same everywhere and live in
//! [`crate::model`] and [`crate::resolve`], so a second provider cannot arrive
//! at a different answer to "which version is newest and compatible" than the
//! first one did.

use async_trait::async_trait;
use ephervex_download::OperationReporter;
use ephervex_model::{LoaderKind, ModProvider};
use tokio_util::sync::CancellationToken;

use crate::error::{ModPlatformError, ModPlatformResult};
use crate::model::{InstanceTarget, ModProject, ModSearchQuery, ModSearchResults, ModVersionInfo};

/// One mod catalog.
#[async_trait]
pub trait ModProviderApi: std::fmt::Debug + Send + Sync {
    /// Which catalog this is.
    fn provider(&self) -> ModProvider;

    /// This provider's own name for a loader, used in queries and in
    /// compatibility checks.
    ///
    /// `None` when the provider has no name for it, which is the honest answer
    /// for Vanilla: an instance with no mod loader cannot host mods, and the
    /// caller is expected to refuse rather than to search for nothing.
    fn loader_name(&self, loader: LoaderKind) -> Option<&'static str>;

    /// Searches for mods, annotated against the instance.
    async fn search(
        &self,
        target: &InstanceTarget,
        query: &ModSearchQuery,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<ModSearchResults>;

    /// One project, by id or by whatever human-typable reference the provider
    /// accepts.
    async fn project(
        &self,
        reference: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<ModProject>;

    /// The versions of a project this instance could be offered.
    ///
    /// `target` is what the answer is *for*, not a filter the caller then has
    /// to reapply: compatibility is still decided here in [`crate::model`], by
    /// one implementation, so no provider can arrive at a different verdict
    /// than another.
    ///
    /// What each provider does with it differs, and the difference is real
    /// rather than incidental. A provider whose whole version list is one
    /// cacheable document returns all of it and lets the local check explain
    /// the versions that do not fit. A provider that pages its file listing
    /// cannot enumerate a decade of releases to answer one question, so it asks
    /// its own API for the release and loader in hand, at the cost of having
    /// nothing left to explain an incompatible version with.
    async fn versions(
        &self,
        project_id: &str,
        target: &InstanceTarget,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<ModVersionInfo>>;

    /// Several projects by id, in one request where the provider allows it.
    ///
    /// A finished plan needs each mod's declared client and server support, and
    /// asking for them one at a time would spend a rate-limit budget on round
    /// trips for a graph the resolver has already settled.
    async fn projects_by_id(
        &self,
        ids: &[String],
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<ModProject>>;

    /// Several versions by id, in one request where the provider allows it.
    ///
    /// Used when a dependency pins an exact version: those arrive in batches,
    /// and asking for them one at a time would spend a rate-limit budget on
    /// round trips.
    async fn versions_by_id(
        &self,
        ids: &[String],
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<ModVersionInfo>>;
}

/// The loader name a target resolves to, or a refusal.
///
/// One definition of "this instance cannot host mods", so the CLI, the services
/// layer, and the provider cannot disagree about it.
pub fn loader_name_for(
    provider: &dyn ModProviderApi,
    target: &InstanceTarget,
) -> ModPlatformResult<&'static str> {
    provider
        .loader_name(target.loader)
        .ok_or(ModPlatformError::LoaderCannotHostMods {
            loader: target.loader,
        })
}
