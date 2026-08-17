//! Talking to Modrinth: endpoints, requests, and the metadata cache.
//!
//! Everything here returns [`super::dto`] shapes. Reading them is
//! [`super::mapping`]'s job, so a question about *what was asked for* and a
//! question about *what the answer means* are answered in different files.
//!
//! # Facets narrow, they do not decide
//!
//! Search takes facets, and it is tempting to treat a facet-filtered result as
//! a list of compatible projects. It is not. A project satisfies
//! `versions:1.20.1` if *any* of its versions does, and `categories:neoforge`
//! if *any* of them does — which can be two different versions.
//!
//! So the facets are a way to ask for fewer candidates, never a verdict. That
//! is also why the two search modes differ: when the caller wants to see
//! incompatible projects, the compatibility facets are *dropped*, because
//! otherwise the provider would filter out precisely the rows this launcher is
//! meant to annotate.
//!
//! # What is cached, and what is not
//!
//! Version and project documents are cached per project, and fetched
//! unfiltered, so the compatible-only view and the show-incompatible view read
//! the same document and a user's answer does not depend on which they opened
//! first. Searches and batch lookups are not cached: a search is per-keystroke
//! and never re-read, and giving each typed query its own cache file would fill
//! the metadata store with documents nothing asks for twice.
//!
//! # Where a cached document lands
//!
//! Every request that touches the cache — including the uncached ones, which
//! still stage a download and drop the entry afterwards — is addressed by the
//! digest of a canonical key built in [`super::cache_key`]. Nothing a user
//! typed and nothing a provider named reaches a path, which is what keeps the
//! filename a fixed length no matter how long a slug or a search query is.

use ephervex_download::{
    CacheLayout, DownloadManager, MetadataCache, MetadataCacheError, MetadataKey,
    OperationReporter, Url,
};
use ephervex_model::ModProvider;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::PROJECT_TYPE;
use super::cache_key::{CompatibilityFilter, ModrinthRequest};
use super::dto::{RawProject, RawSearch, RawVersion};
use crate::error::{ModPlatformError, ModPlatformResult};
use crate::model::{InstanceTarget, MAX_SEARCH_LIMIT, ModSearchQuery};

/// The production API root.
pub const MODRINTH_API_BASE: &str = "https://api.modrinth.com/v2";

/// Modrinth's HTTP surface, as much of it as this launcher uses.
#[derive(Debug, Clone)]
pub(super) struct ModrinthApi {
    manager: DownloadManager,
    cache: MetadataCache,
    base: Url,
}

impl ModrinthApi {
    /// An API against an explicit root.
    ///
    /// The root is a parameter so integration tests can point the whole
    /// provider at a loopback server, the same way `ephervex-loaders` does for
    /// its metadata sources. Production always uses [`MODRINTH_API_BASE`], and
    /// the download manager's transport policy still refuses plaintext HTTP
    /// unless a test has explicitly opted in.
    pub(super) fn new(
        manager: DownloadManager,
        layout: CacheLayout,
        base: &str,
    ) -> ModPlatformResult<Self> {
        let base = Url::parse(&format!("{}/", base.trim_end_matches('/'))).map_err(|error| {
            ModPlatformError::ResponseInvalid {
                provider: ModProvider::Modrinth,
                detail: error.to_string(),
            }
        })?;
        Ok(Self {
            manager,
            cache: MetadataCache::new(layout),
            base,
        })
    }

    /// One page of search results.
    pub(super) async fn search(
        &self,
        target: &InstanceTarget,
        query: &ModSearchQuery,
        loader: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<RawSearch> {
        let limit = query.limit.clamp(1, MAX_SEARCH_LIMIT);
        let url = self.endpoint(
            &["search"],
            &[
                ("query", query.text.clone()),
                ("facets", facets(target, query, loader)),
                ("limit", limit.to_string()),
                ("offset", query.offset.to_string()),
            ],
        )?;

        let response: RawSearch = self
            .fetch_uncached(
                &ModrinthRequest::Search {
                    text: &query.text,
                    // The clamped value, because that is the page the provider
                    // is actually asked for.
                    limit,
                    offset: query.offset,
                    filter: (!query.include_incompatible).then_some(CompatibilityFilter {
                        minecraft_version: &target.minecraft_version,
                        loader,
                    }),
                },
                &url,
                reporter,
                cancel,
            )
            .await?;
        debug!(
            hits = response.hits.len(),
            total = response.total_hits,
            "searched Modrinth"
        );
        Ok(response)
    }

    /// One project, by id or slug.
    pub(super) async fn project(
        &self,
        reference: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<RawProject> {
        let url = self.endpoint(&["project", reference], &[])?;
        self.fetch(
            &ModrinthRequest::Project { reference },
            &url,
            reporter,
            cancel,
        )
        .await
        .map_err(|error| project_unknown(error, reference))
    }

    /// Every version of a project, unfiltered.
    pub(super) async fn versions(
        &self,
        project_id: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<RawVersion>> {
        let url = self.endpoint(&["project", project_id, "version"], &[])?;
        self.fetch(
            &ModrinthRequest::ProjectVersions { project_id },
            &url,
            reporter,
            cancel,
        )
        .await
        .map_err(|error| project_unknown(error, project_id))
    }

    /// Several projects in one request.
    pub(super) async fn projects_by_id(
        &self,
        ids: &[String],
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<RawProject>> {
        let url = self.endpoint(&["projects"], &[("ids", json_id_array(ids))])?;
        self.fetch_uncached(&ModrinthRequest::Projects { ids }, &url, reporter, cancel)
            .await
    }

    /// Several versions in one request.
    pub(super) async fn versions_by_id(
        &self,
        ids: &[String],
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<Vec<RawVersion>> {
        let url = self.endpoint(&["versions"], &[("ids", json_id_array(ids))])?;
        self.fetch_uncached(&ModrinthRequest::Versions { ids }, &url, reporter, cancel)
            .await
    }

    /// Builds an endpoint URL from a path and query.
    ///
    /// Every dynamic segment goes through `Url`'s own escaping, so a project
    /// reference taken from a command line cannot smuggle a path.
    fn endpoint(&self, segments: &[&str], query: &[(&str, String)]) -> ModPlatformResult<Url> {
        let mut url = self.base.clone();
        {
            let mut path =
                url.path_segments_mut()
                    .map_err(|()| ModPlatformError::ResponseInvalid {
                        provider: ModProvider::Modrinth,
                        detail: "the api base cannot have path segments".to_owned(),
                    })?;
            // The base is normalized with a trailing slash, which leaves an
            // empty final segment; appending onto it would produce `//`.
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
        }
        Ok(url)
    }

    /// Fetches a document, serving a cached copy while the provider is down.
    async fn fetch<T: serde::de::DeserializeOwned>(
        &self,
        request: &ModrinthRequest<'_>,
        url: &Url,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<T> {
        let key = cache_key(request)?;
        let document = self
            .cache
            .fetch(&self.manager, &key, url, reporter, cancel)
            .await
            .map_err(unreachable)?;
        document
            .parse()
            .map_err(|error| ModPlatformError::ResponseInvalid {
                provider: ModProvider::Modrinth,
                detail: error.to_string(),
            })
    }

    /// Fetches a document that is not worth caching.
    ///
    /// The entry is still addressed by the request rather than by the endpoint,
    /// so two of these running at once stage their downloads at different paths
    /// instead of over each other.
    async fn fetch_uncached<T: serde::de::DeserializeOwned>(
        &self,
        request: &ModrinthRequest<'_>,
        url: &Url,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<T> {
        let key = cache_key(request)?;
        let document = self
            .cache
            .refresh(&self.manager, &key, url, reporter, cancel)
            .await
            .map_err(unreachable)?;
        let parsed = document
            .parse()
            .map_err(|error| ModPlatformError::ResponseInvalid {
                provider: ModProvider::Modrinth,
                detail: error.to_string(),
            });
        self.cache.invalidate(&key).await;
        parsed
    }
}

/// The facets one search sends.
///
/// The compatibility facets are dropped when the caller wants incompatible
/// results, because a filter cannot both narrow the list and leave the rows
/// that need annotating in it.
fn facets(target: &InstanceTarget, query: &ModSearchQuery, loader: &str) -> String {
    let mut groups = vec![format!("[\"project_type:{PROJECT_TYPE}\"]")];
    if !query.include_incompatible {
        groups.push(format!("[\"categories:{loader}\"]"));
        groups.push(format!("[\"versions:{}\"]", target.minecraft_version));
    }
    format!("[{}]", groups.join(","))
}

/// Where one request's answer is cached.
fn cache_key(request: &ModrinthRequest<'_>) -> ModPlatformResult<MetadataKey> {
    request
        .cache_key()
        .map_err(|error| ModPlatformError::ResponseInvalid {
            provider: ModProvider::Modrinth,
            detail: error.to_string(),
        })
}

/// Modrinth encodes id lists as a JSON array inside a query parameter.
fn json_id_array(ids: &[String]) -> String {
    let quoted: Vec<String> = ids
        .iter()
        .map(|id| serde_json::Value::String(id.clone()).to_string())
        .collect();
    format!("[{}]", quoted.join(","))
}

/// Turns a metadata failure into this crate's own.
fn unreachable(error: MetadataCacheError) -> ModPlatformError {
    match error {
        MetadataCacheError::NotJson { key, source } => ModPlatformError::ResponseInvalid {
            provider: ModProvider::Modrinth,
            detail: format!("{key}: {source}"),
        },
        other => ModPlatformError::Unreachable {
            provider: ModProvider::Modrinth,
            source: Box::new(other),
        },
    }
}

/// A 404 from a project endpoint means the reference is wrong, which is worth
/// saying rather than reporting as an outage.
fn project_unknown(error: ModPlatformError, reference: &str) -> ModPlatformError {
    if error.is_offline() {
        return ModPlatformError::ProjectUnknown {
            provider: ModProvider::Modrinth,
            reference: reference.to_owned(),
        };
    }
    error
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::LoaderKind;

    use super::*;

    fn target() -> InstanceTarget {
        InstanceTarget::new("1.20.1", LoaderKind::NeoForge)
    }

    #[test]
    fn a_compatible_only_search_narrows_on_the_exact_release_and_loader() {
        let facets = facets(&target(), &ModSearchQuery::new("jei"), "neoforge");
        assert_eq!(
            facets,
            r#"[["project_type:mod"],["categories:neoforge"],["versions:1.20.1"]]"#
        );
    }

    #[test]
    fn showing_incompatible_results_drops_the_compatibility_facets() {
        // Otherwise the provider filters out exactly the rows this launcher was
        // asked to annotate.
        let query = ModSearchQuery::new("jei").including_incompatible(true);
        let facets = facets(&target(), &query, "neoforge");
        assert_eq!(facets, r#"[["project_type:mod"]]"#);
        assert!(!facets.contains("versions:"));
        assert!(!facets.contains("categories:"));
    }

    #[test]
    fn an_id_taken_from_a_command_line_cannot_escape_the_metadata_store() {
        // Nothing of the reference survives into the address, so a separator or
        // a `..` in one is not a path component to begin with.
        for hostile in ["../../etc", "a/b", "", "..", "\0"] {
            let key = cache_key(&ModrinthRequest::Project { reference: hostile })
                .expect("a reference is hashed, never spelled out");
            assert!(
                !key.as_str().contains(".."),
                "`{hostile}` reached the address as `{key}`"
            );
            assert!(
                MetadataKey::new(
                    key.as_str()
                        .rsplit('/')
                        .next()
                        .expect("an address has a final segment")
                )
                .is_ok(),
                "`{hostile}` produced an unusable final segment"
            );
        }
    }

    #[test]
    fn id_lists_are_encoded_the_way_the_api_documents() {
        assert_eq!(
            json_id_array(&["AABBCCDD".to_owned(), "EEFFGGHH".to_owned()]),
            r#"["AABBCCDD","EEFFGGHH"]"#
        );
        assert_eq!(json_id_array(&[]), "[]");
    }
}
