//! End-to-end behavior of [`VersionCatalog`] against a loopback HTTP server
//!
//! These cover the paths that unit tests over a fixture string cannot: what
//! happens when Mojang is slow, wrong, or gone. Nothing here touches a live
//! service - the fixtures are embedded at compile time and the "provider" is a
//! `wiremock` server on localhost, so `cargo test --offline` passes with the
//! network unplugged.
//!
//! The manifest fixture's URLs are rewritten to point at the mock server, which
//! is also a small proof in itself: the launcher follows the URLs the manifest
//! gives it and never reconstructs one from a version id.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use ephervex_download::{
    CacheLayout, ContentStore, DownloadManager, DownloaderConfig, FileExtension, Freshness,
    HttpConfig, MetadataCache, OperationReporter,
};
use ephervex_minecraft::{MinecraftError, VersionCatalog};
use ephervex_model::{Hash, HashAlgorithm};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MANIFEST: &str = include_str!("../../../fixtures/minecraft/version_manifest_v2.json");
const V1_12_2: &str = include_str!("../../../fixtures/minecraft/1.12.2.json");
const V1_20_1: &str = include_str!("../../../fixtures/minecraft/1.20.1.json");
const V1_21_1: &str = include_str!("../../../fixtures/minecraft/1.21.1.json");
const V26_1: &str = include_str!("../../../fixtures/minecraft/26.1.json");

/// The host the captured manifest names, rewritten to the mock server so the
/// per-version URLs in the fixture resolve to localhost.
const PISTON_META: &str = "https://piston-meta.mojang.com";

const MANIFEST_PATH: &str = "/mc/game/version_manifest_v2.json";

/// The four era fixtures, with the path each one's manifest entry points at.
fn version_documents() -> [(&'static str, &'static str, &'static str); 4] {
    [
        (
            "1.12.2",
            "/v1/packages/832d95b9f40699d4961394dcf6cf549e65f15dc5/1.12.2.json",
            V1_12_2,
        ),
        (
            "1.20.1",
            "/v1/packages/c2f76c955503c7143424b51e21eea87bbe5c6547/1.20.1.json",
            V1_20_1,
        ),
        (
            "1.21.1",
            "/v1/packages/091b2c2728f8ac413b6e7e683a6c12f21a53057c/1.21.1.json",
            V1_21_1,
        ),
        (
            "26.1",
            "/v1/packages/d6e4071ad771d30003e17727e8acd41283aef208/26.1.json",
            V26_1,
        ),
    ]
}

fn served_manifest(server: &MockServer) -> String {
    MANIFEST.replace(PISTON_META, &server.uri())
}

async fn mount_manifest(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(MANIFEST_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(served_manifest(server)))
        .mount(server)
        .await;
}

async fn mount_versions(server: &MockServer) {
    for (_, route, body) in version_documents() {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(server)
            .await;
    }
}

fn manager() -> DownloadManager {
    DownloadManager::new(DownloaderConfig {
        http: HttpConfig::for_loopback_tests(),
        ..DownloaderConfig::default()
    })
    .expect("client builds")
}

fn catalog(server: &MockServer, cache: &tempfile::TempDir) -> VersionCatalog {
    VersionCatalog::new(
        manager(),
        CacheLayout::new(cache.path()),
        &format!("{}{MANIFEST_PATH}", server.uri()),
    )
    .expect("catalog builds")
}

fn reporter() -> OperationReporter {
    OperationReporter::silent("minecraft-metadata")
}

async fn requests_to(server: &MockServer, route: &str) -> usize {
    server
        .received_requests()
        .await
        .expect("request log")
        .iter()
        .filter(|request| request.url.path() == route)
        .count()
}

#[tokio::test]
async fn the_catalog_lists_only_supported_releases_from_a_freshly_fetched_manifest() {
    let server = MockServer::start().await;
    mount_manifest(&server).await;
    let cache = tempfile::tempdir().expect("temp dir");

    let listing = catalog(&server, &cache)
        .supported_releases(&reporter(), &CancellationToken::new())
        .await
        .expect("listing");

    let ids: Vec<&str> = listing
        .releases
        .iter()
        .map(|record| record.id.as_str())
        .collect();
    assert_eq!(ids, ["26.2", "26.1", "1.21.1", "1.20.1", "1.12.2"]);
    assert_eq!(listing.freshness, Freshness::Fetched);
    assert_eq!(listing.latest.release, "26.2");
    assert_eq!(listing.total_versions, 14);
    assert_eq!(listing.unrecognized_version_ids().count(), 0);
}

#[tokio::test]
async fn a_second_listing_inside_the_ttl_makes_no_request_at_all() {
    let server = MockServer::start().await;
    mount_manifest(&server).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let catalog = catalog(&server, &cache);

    let first = catalog
        .supported_releases(&reporter(), &CancellationToken::new())
        .await
        .expect("first listing");
    let second = catalog
        .supported_releases(&reporter(), &CancellationToken::new())
        .await
        .expect("second listing");

    assert_eq!(first.freshness, Freshness::Fetched);
    assert_eq!(second.freshness, Freshness::Cached);
    assert_eq!(first.releases, second.releases);
    assert_eq!(requests_to(&server, MANIFEST_PATH).await, 1);
}

#[tokio::test]
async fn an_expired_manifest_is_served_stale_when_the_refresh_fails() {
    let server = MockServer::start().await;
    mount_manifest(&server).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let layout = CacheLayout::new(cache.path());

    // Populate the cache while the provider is healthy.
    catalog(&server, &cache)
        .supported_releases(&reporter(), &CancellationToken::new())
        .await
        .expect("first listing");

    // Now the provider breaks, and the cached copy is already past its TTL.
    server.reset().await;
    Mock::given(method("GET"))
        .and(path(MANIFEST_PATH))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let expired =
        catalog(&server, &cache).with_cache(MetadataCache::with_ttl(layout, Duration::ZERO));
    let listing = expired
        .supported_releases(&reporter(), &CancellationToken::new())
        .await
        .expect("a flaky provider must not stop the launcher listing what it knows");

    assert!(listing.is_stale());
    assert_eq!(listing.freshness, Freshness::Stale);
    assert_eq!(
        listing.releases.len(),
        5,
        "the known list is still complete"
    );
}

#[tokio::test]
async fn an_unreachable_provider_with_nothing_cached_is_an_error_not_a_spinner() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(MANIFEST_PATH))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let cache = tempfile::tempdir().expect("temp dir");

    let error = catalog(&server, &cache)
        .supported_releases(&reporter(), &CancellationToken::new())
        .await
        .expect_err("no cache and no provider is a failure the caller must see");

    assert!(matches!(error, MinecraftError::ManifestUnavailable { .. }));
    assert_eq!(
        error.message_id(),
        "error-minecraft-manifest-unavailable",
        "the frontend must be able to tell this apart from a stale list"
    );
}

#[tokio::test]
async fn every_era_resolves_to_complete_vanilla_metadata() {
    let server = MockServer::start().await;
    mount_manifest(&server).await;
    mount_versions(&server).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let catalog = catalog(&server, &cache);

    for (id, _, _) in version_documents() {
        let version = catalog
            .resolve(id, &reporter(), &CancellationToken::new())
            .await
            .unwrap_or_else(|error| panic!("`{id}` should resolve: {error}"));

        assert_eq!(version.id, id);
        assert!(version.is_complete_vanilla(), "{id}");
        assert!(version.client_jar().is_ok(), "{id}");
        assert!(version.asset_index().is_ok(), "{id}");
        assert!(version.java_major_version().is_some(), "{id}");
        assert!(!version.libraries.is_empty(), "{id}");
    }
}

#[tokio::test]
async fn a_version_document_is_fetched_once_and_never_again() {
    let server = MockServer::start().await;
    mount_manifest(&server).await;
    mount_versions(&server).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let catalog = catalog(&server, &cache);
    let route = version_documents()[0].1;

    for _ in 0..3 {
        catalog
            .resolve("1.12.2", &reporter(), &CancellationToken::new())
            .await
            .expect("resolve");
    }

    assert_eq!(
        requests_to(&server, route).await,
        1,
        "a published version document is immutable and content-addressed"
    );
}

#[tokio::test]
async fn a_version_document_that_fails_its_digest_is_refused_rather_than_used() {
    let server = MockServer::start().await;
    mount_manifest(&server).await;
    let (id, route, body) = version_documents()[0];
    Mock::given(method("GET"))
        .and(path(route))
        // Valid metadata, wrong bytes: the digest the manifest published is
        // the only thing that can tell the difference.
        .respond_with(ResponseTemplate::new(200).set_body_string(body.replace("1.12.2", "1.12.1")))
        .mount(&server)
        .await;
    let cache = tempfile::tempdir().expect("temp dir");

    let error = catalog(&server, &cache)
        .resolve(id, &reporter(), &CancellationToken::new())
        .await
        .expect_err("a document that does not match its sha1 must not be accepted");

    assert!(matches!(
        error,
        MinecraftError::VersionMetadataCorrupt { .. }
    ));
}

#[tokio::test]
async fn an_oversized_version_document_is_abandoned_before_it_is_stored() {
    let server = MockServer::start().await;
    mount_manifest(&server).await;
    let (id, route, _) = version_documents()[1];
    // Far past any real version document, and not the bytes the manifest's
    // digest names: the size ceiling has to fire before the digest can.
    let oversized = vec![b' '; 9 * 1024 * 1024];
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(oversized))
        .mount(&server)
        .await;
    let cache = tempfile::tempdir().expect("temp dir");

    let error = catalog(&server, &cache)
        .resolve(id, &reporter(), &CancellationToken::new())
        .await
        .expect_err("a body past the ceiling must not be accepted");

    assert!(
        matches!(error, MinecraftError::VersionMetadataUnavailable { .. }),
        "the ceiling must reject the body, not the digest check: {error:?}"
    );
    let digest = "c2f76c955503c7143424b51e21eea87bbe5c6547";
    assert!(
        !files_under(cache.path()).any(|file| file.to_string_lossy().contains(digest)),
        "nothing may be published under the document's digest"
    );
}

/// Every regular file below `root`.
fn files_under(root: &std::path::Path) -> impl Iterator<Item = std::path::PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).expect("readable directory") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.into_iter()
}

#[tokio::test]
async fn resolving_something_outside_the_supported_set_fails_clearly() {
    let server = MockServer::start().await;
    mount_manifest(&server).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let catalog = catalog(&server, &cache);
    let cancel = CancellationToken::new();

    let too_old = catalog
        .resolve("1.11.2", &reporter(), &cancel)
        .await
        .expect_err("below the floor");
    assert!(matches!(
        too_old,
        MinecraftError::VersionUnsupported {
            minimum: "1.12.2",
            ..
        }
    ));

    let snapshot = catalog
        .resolve("26.2-rc-1", &reporter(), &cancel)
        .await
        .expect_err("a release candidate is not a release");
    assert!(
        matches!(snapshot, MinecraftError::VersionNotARelease { .. }),
        "a release candidate is refused for being the wrong channel, not for being old"
    );

    let unknown = catalog
        .resolve("1.20.99", &reporter(), &cancel)
        .await
        .expect_err("not in the manifest");
    assert!(matches!(unknown, MinecraftError::UnknownVersion { .. }));
}

#[tokio::test]
async fn a_manifest_that_is_not_a_manifest_is_reported_as_such() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(MANIFEST_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"latest":{}}"#))
        .mount(&server)
        .await;
    let cache = tempfile::tempdir().expect("temp dir");

    let error = catalog(&server, &cache)
        .supported_releases(&reporter(), &CancellationToken::new())
        .await
        .expect_err("a well-formed but wrong document is not a version manifest");

    assert!(matches!(error, MinecraftError::ManifestInvalid { .. }));
}

/// The digest the manifest fixture publishes for the 1.12.2 version document.
const V1_12_2_SHA1: &str = "4cb18237b1ae67d67ce1d537926a552453c91d57";

/// Where `resolve` stored the 1.12.2 version document, found the same way the
/// catalog finds it: by the digest the manifest published.
fn cached_document_path(cache: &tempfile::TempDir) -> std::path::PathBuf {
    CacheLayout::new(cache.path()).content(
        ContentStore::Metadata,
        &Hash::new(HashAlgorithm::Sha1, V1_12_2_SHA1).expect("fixture digest"),
        Some(&FileExtension::new("json").expect("json extension")),
    )
}

#[tokio::test]
async fn a_cached_version_document_is_returned_offline_when_it_matches_its_digest() {
    let server = MockServer::start().await;
    mount_manifest(&server).await;
    mount_versions(&server).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let catalog = catalog(&server, &cache);
    catalog
        .resolve("1.12.2", &reporter(), &CancellationToken::new())
        .await
        .expect("resolve");

    let cached = catalog
        .cached_metadata("1.12.2")
        .await
        .expect("an intact cached document is trusted")
        .expect("the document was cached by resolve");

    assert_eq!(cached.main_class, "net.minecraft.client.main.Main");
}

#[tokio::test]
async fn a_cached_version_document_that_was_altered_in_place_is_reported_corrupt() {
    let server = MockServer::start().await;
    mount_manifest(&server).await;
    mount_versions(&server).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let catalog = catalog(&server, &cache);
    catalog
        .resolve("1.12.2", &reporter(), &CancellationToken::new())
        .await
        .expect("resolve");
    let stored = cached_document_path(&cache);
    // Still valid metadata under the same file name; only the digest can tell.
    let original = std::fs::read_to_string(&stored).expect("cached document");
    std::fs::write(
        &stored,
        original.replace("net.minecraft.client.main.Main", "evil.Main"),
    )
    .expect("tamper with the cached document");

    let error = catalog
        .cached_metadata("1.12.2")
        .await
        .expect_err("an altered document must not be returned as metadata");

    assert!(matches!(
        error,
        MinecraftError::VersionMetadataCorrupt { .. }
    ));
}

#[tokio::test]
async fn an_oversized_cached_version_document_is_reported_corrupt_without_being_parsed() {
    let server = MockServer::start().await;
    mount_manifest(&server).await;
    mount_versions(&server).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let catalog = catalog(&server, &cache);
    catalog
        .resolve("1.12.2", &reporter(), &CancellationToken::new())
        .await
        .expect("resolve");
    std::fs::write(cached_document_path(&cache), vec![b' '; 9 * 1024 * 1024])
        .expect("replace the cached document");

    let error = catalog
        .cached_metadata("1.12.2")
        .await
        .expect_err("a document past the size ceiling cannot be the published one");

    assert!(matches!(
        error,
        MinecraftError::VersionMetadataCorrupt { .. }
    ));
}

#[tokio::test]
async fn a_version_document_that_was_never_cached_is_reported_as_not_cached() {
    let server = MockServer::start().await;
    mount_manifest(&server).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let catalog = catalog(&server, &cache);
    catalog
        .supported_releases(&reporter(), &CancellationToken::new())
        .await
        .expect("listing caches the manifest");

    let cached = catalog
        .cached_metadata("1.12.2")
        .await
        .expect("a missing document is not an error");

    assert!(cached.is_none());
}
