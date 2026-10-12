//! Per-release language catalogs, against a loopback HTTP server.
//!
//! The catalog is three hops from a version id: the version document names an
//! asset index, the index names the `pack.mcmeta` object, and that object's
//! `language` section is the list. These tests drive all three, plus what
//! happens when the last one is missing, truncated, or served by a host that
//! has since gone away.
//!
//! Nothing here touches a live service - the "provider" is a `wiremock` server
//! on localhost, so `cargo test --offline` passes with the network unplugged.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use ephervex_download::{
    CacheLayout, DownloadManager, DownloaderConfig, HttpConfig, MetadataCache, OperationReporter,
};
use ephervex_minecraft::{LanguageCatalogs, MinecraftError, VersionCatalog};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MANIFEST: &str = include_str!("../../../fixtures/minecraft/version_manifest_v2.json");
const V1_20_1: &str = include_str!("../../../fixtures/minecraft/1.20.1.json");

const PISTON_META: &str = "https://piston-meta.mojang.com";
const MANIFEST_PATH: &str = "/mc/game/version_manifest_v2.json";
const VERSION_PATH: &str = "/v1/packages/c2f76c955503c7143424b51e21eea87bbe5c6547/1.20.1.json";

/// The `language` section as Mojang publishes it, reduced to three entries.
const PACK_METADATA: &str = r#"{"pack":{"pack_format":15},"language":{
  "en_us":{"name":"English","region":"United States","bidirectional":false},
  "de_de":{"name":"Deutsch","region":"Deutschland"},
  "ar_sa":{"name":"العربية","region":"العالم العربي","bidirectional":true}
}}"#;

fn manager() -> DownloadManager {
    DownloadManager::new(DownloaderConfig {
        http: HttpConfig::for_loopback_tests(),
        ..DownloaderConfig::default()
    })
    .expect("client builds")
}

fn reporter() -> OperationReporter {
    OperationReporter::silent("minecraft-language-catalog")
}

fn sha1(body: &str) -> String {
    use sha1::{Digest, Sha1};
    hex::encode(Sha1::digest(body.as_bytes()))
}

/// The asset index for the version document, naming one object: the resource
/// pack metadata whose `language` section is the catalog.
fn asset_index(body: &str, declared_size: usize) -> String {
    serde_json::json!({
        "objects": {
            "pack.mcmeta": {"hash": sha1(body), "size": declared_size},
            "minecraft/sounds/ambient.ogg": {"hash": sha1("unrelated"), "size": 9},
        }
    })
    .to_string()
}

/// Serves a manifest, the 1.20.1 version document rewritten to name this
/// server's asset index, that index, and the object it points at.
async fn mount(server: &MockServer, pack_metadata: &str) {
    mount_declaring(server, pack_metadata, 0, 0).await;
}

/// Like [`mount`], but the version document and the asset index each declare a
/// size that many bytes short of what is served. The digests still match the
/// served bytes, so only the declared size can tell the difference.
async fn mount_declaring(
    server: &MockServer,
    pack_metadata: &str,
    index_shortfall: usize,
    object_shortfall: usize,
) {
    let index = asset_index(pack_metadata, pack_metadata.len() - object_shortfall);
    let index_sha1 = sha1(&index);
    let index_path = format!("/assets/{index_sha1}.json");

    let version = rewrite_asset_index(
        &V1_20_1.replace(PISTON_META, &server.uri()),
        &format!("{}{index_path}", server.uri()),
        &index_sha1,
        index.len() - index_shortfall,
    );

    Mock::given(method("GET"))
        .and(path(MANIFEST_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(served_manifest(server, &version)))
        .mount(server)
        .await;
    // The manifest publishes the version document's digest, so the served
    // document has to be the one it names.
    Mock::given(method("GET"))
        .and(path(VERSION_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(version))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(index_path))
        .respond_with(ResponseTemplate::new(200).set_body_string(index))
        .mount(server)
        .await;

    let digest = sha1(pack_metadata);
    Mock::given(method("GET"))
        .and(path(format!("/{}/{digest}", &digest[..2])))
        .respond_with(ResponseTemplate::new(200).set_body_string(pack_metadata.to_owned()))
        .mount(server)
        .await;
}

/// The manifest fixture, rewritten to this server and to the digest of the
/// version document it will actually serve.
///
/// The digest is the only thing standing between the launcher and an unverified
/// version document, so a test that changes the document has to update the
/// manifest that vouches for it rather than weaken the check.
fn served_manifest(server: &MockServer, version: &str) -> String {
    let mut manifest: serde_json::Value =
        serde_json::from_str(&MANIFEST.replace(PISTON_META, &server.uri())).expect("manifest");
    for entry in manifest["versions"].as_array_mut().expect("versions") {
        if entry["id"] == "1.20.1" {
            entry["sha1"] = serde_json::Value::String(sha1(version));
        }
    }
    manifest.to_string()
}

/// Replaces the version document's `assetIndex` block with one naming the
/// index this test serves.
fn rewrite_asset_index(version: &str, url: &str, digest: &str, size: usize) -> String {
    let mut document: serde_json::Value = serde_json::from_str(version).expect("version document");
    document["assetIndex"] = serde_json::json!({
        "id": "5",
        "sha1": digest,
        "size": size,
        "totalSize": 1_024,
        "url": url,
    });
    document.to_string()
}

/// A catalog reader whose manifest, version documents, and asset objects all
/// come from the mock server.
fn catalogs(server: &MockServer, cache: &tempfile::TempDir) -> LanguageCatalogs {
    let layout = CacheLayout::new(cache.path());
    let versions = VersionCatalog::new(
        manager(),
        layout.clone(),
        &format!("{}{MANIFEST_PATH}", server.uri()),
    )
    .expect("catalog builds");
    LanguageCatalogs::new(versions, manager(), layout).with_assets_base(&server.uri())
}

/// Resolves 1.20.1's catalog through the whole chain: manifest, version
/// document, asset index, object.
async fn resolve(
    server: &MockServer,
    cache: &tempfile::TempDir,
) -> ephervex_minecraft::LanguageCatalog {
    catalogs(server, cache)
        .resolve("1.20.1", &reporter(), &CancellationToken::new())
        .await
        .expect("the catalog resolves")
}

#[tokio::test]
async fn a_catalog_is_resolved_from_the_releases_own_asset_metadata_and_cached() {
    let server = MockServer::start().await;
    mount(&server, PACK_METADATA).await;
    let cache = tempfile::tempdir().expect("temp dir");

    let catalog = resolve(&server, &cache).await;
    assert_eq!(catalog.minecraft_version, "1.20.1");
    assert_eq!(catalog.asset_index_id, "5");
    assert_eq!(
        catalog.codes().collect::<Vec<_>>(),
        ["ar_sa", "de_de", "en_us"]
    );
    assert!(catalog.entry("ar_sa").expect("present").bidirectional);
    assert_eq!(
        catalog.entry("de_de").expect("present").region,
        "Deutschland"
    );

    // Cached under the release, so the next question is answered offline.
    let cached = catalogs(&server, &cache)
        .cached("1.20.1")
        .await
        .expect("cached under its release");
    assert_eq!(cached, catalog);
}

#[tokio::test]
async fn a_cached_catalog_answers_after_the_host_is_gone() {
    let server = MockServer::start().await;
    mount(&server, PACK_METADATA).await;
    let cache = tempfile::tempdir().expect("temp dir");
    resolve(&server, &cache).await;
    drop(server);

    let offline = MockServer::start().await;
    let catalogs = catalogs(&offline, &cache);
    let cached = catalogs
        .resolve("1.20.1", &reporter(), &CancellationToken::new())
        .await
        .expect("a fresh cache needs no network at all");
    assert!(cached.contains("de_de"));
}

#[tokio::test]
async fn language_support_is_answered_per_release_rather_than_assumed() {
    let server = MockServer::start().await;
    mount(&server, PACK_METADATA).await;
    let cache = tempfile::tempdir().expect("temp dir");

    let catalog = resolve(&server, &cache).await;
    assert!(catalog.contains("de_de"));
    assert!(
        !catalog.contains("qya_aa"),
        "a code this release does not offer must not be claimed"
    );

    // A release nobody has resolved is unknown, not empty: no catalog at all is
    // a different answer from a catalog with nothing in it.
    assert!(catalogs(&server, &cache).cached("1.12.2").await.is_none());
}

#[tokio::test]
async fn a_pack_metadata_object_that_is_not_a_catalog_leaves_the_release_unresolved() {
    // A truncated download, an error page, or metadata with no language
    // section. Reporting "zero languages" would read as a release that offers
    // none, which no Minecraft release has ever done.
    for body in [
        r#"{"pack":{"pack_format":15}}"#,
        r#"{"language":{}}"#,
        "<html>503</html>",
    ] {
        let server = MockServer::start().await;
        mount(&server, body).await;
        let cache = tempfile::tempdir().expect("temp dir");

        let error = catalogs(&server, &cache)
            .resolve("1.20.1", &reporter(), &CancellationToken::new())
            .await
            .expect_err("an unusable document must not become a catalog");
        assert_eq!(
            error.message_id(),
            "error-minecraft-language-catalog-unavailable",
            "{body}"
        );
        assert!(catalogs(&server, &cache).cached("1.20.1").await.is_none());
    }
}

#[tokio::test]
async fn an_asset_index_longer_than_its_published_size_is_refused() {
    let server = MockServer::start().await;
    mount_declaring(&server, PACK_METADATA, 1, 0).await;
    let cache = tempfile::tempdir().expect("temp dir");

    let error = catalogs(&server, &cache)
        .resolve("1.20.1", &reporter(), &CancellationToken::new())
        .await
        .expect_err("an index that overruns its published size must not be accepted");

    assert!(matches!(
        error,
        MinecraftError::VersionMetadataUnavailable { .. }
    ));
    assert!(catalogs(&server, &cache).cached("1.20.1").await.is_none());
}

#[tokio::test]
async fn a_pack_metadata_object_longer_than_its_published_size_is_refused() {
    let server = MockServer::start().await;
    mount_declaring(&server, PACK_METADATA, 0, 1).await;
    let cache = tempfile::tempdir().expect("temp dir");

    let error = catalogs(&server, &cache)
        .resolve("1.20.1", &reporter(), &CancellationToken::new())
        .await
        .expect_err("an object that overruns its published size must not be accepted");

    assert!(matches!(
        error,
        MinecraftError::VersionMetadataUnavailable { .. }
    ));
    assert!(catalogs(&server, &cache).cached("1.20.1").await.is_none());
}

#[tokio::test]
async fn a_stale_cache_is_refreshed_and_the_refreshed_copy_replaces_it_atomically() {
    let server = MockServer::start().await;
    mount(&server, PACK_METADATA).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let layout = CacheLayout::new(cache.path());

    resolve(&server, &cache).await;

    // A zero TTL makes what is on disk expired, which is the same condition as
    // a catalog that has simply been sitting there.
    let expiring =
        catalogs(&server, &cache).with_cache(MetadataCache::with_ttl(layout, Duration::ZERO));
    let refreshed = expiring
        .resolve("1.20.1", &reporter(), &CancellationToken::new())
        .await
        .expect("a stale entry is refreshed rather than served");
    assert!(refreshed.contains("de_de"));
    assert!(
        server.received_requests().await.expect("log").len() > 3,
        "the stale entry was served instead of being refreshed"
    );
}

/// A resolver whose language cache is `cache` and expired, but whose version
/// catalog has its own empty cache and an unreachable provider, so a refresh
/// cannot succeed.
fn expired_with_unreachable_provider(
    unreachable: &MockServer,
    cache: &tempfile::TempDir,
) -> (LanguageCatalogs, tempfile::TempDir) {
    let versions_cache = tempfile::tempdir().expect("temp dir");
    let versions = VersionCatalog::new(
        manager(),
        CacheLayout::new(versions_cache.path()),
        &format!("{}{MANIFEST_PATH}", unreachable.uri()),
    )
    .expect("catalog builds");
    let layout = CacheLayout::new(cache.path());
    let catalogs = LanguageCatalogs::new(versions, manager(), layout.clone())
        .with_assets_base(&unreachable.uri())
        .with_cache(MetadataCache::with_ttl(layout, Duration::ZERO));
    (catalogs, versions_cache)
}

#[tokio::test]
async fn an_expired_catalog_is_served_when_the_refresh_fails() {
    let server = MockServer::start().await;
    mount(&server, PACK_METADATA).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let fetched = resolve(&server, &cache).await;
    drop(server);

    let unreachable = MockServer::start().await;
    let (catalogs, _versions_cache) = expired_with_unreachable_provider(&unreachable, &cache);
    let answer = catalogs
        .resolve("1.20.1", &reporter(), &CancellationToken::new())
        .await
        .expect("the retained catalog answers when the provider cannot");

    assert_eq!(answer, fetched);
    assert!(
        !unreachable
            .received_requests()
            .await
            .expect("log")
            .is_empty(),
        "the expired catalog was served without attempting a refresh"
    );
}

#[tokio::test]
async fn a_canceled_refresh_is_reported_instead_of_serving_the_expired_catalog() {
    let server = MockServer::start().await;
    mount(&server, PACK_METADATA).await;
    let cache = tempfile::tempdir().expect("temp dir");
    resolve(&server, &cache).await;
    drop(server);

    let unreachable = MockServer::start().await;
    let (catalogs, _versions_cache) = expired_with_unreachable_provider(&unreachable, &cache);
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = catalogs
        .resolve("1.20.1", &reporter(), &cancel)
        .await
        .expect_err("a cancellation must not be answered with stale data");
    assert!(error.is_cancellation(), "{error:?}");
}

#[tokio::test]
async fn a_failed_refresh_without_a_retained_catalog_is_reported() {
    let unreachable = MockServer::start().await;
    let cache = tempfile::tempdir().expect("temp dir");
    let (catalogs, _versions_cache) = expired_with_unreachable_provider(&unreachable, &cache);

    let error = catalogs
        .resolve("1.20.1", &reporter(), &CancellationToken::new())
        .await
        .expect_err("nothing is cached to fall back on");
    assert!(matches!(error, MinecraftError::ManifestUnavailable { .. }));
}
