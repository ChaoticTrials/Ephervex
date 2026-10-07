//! The remote options compatibility feed, against a loopback HTTP server.
//!
//! These cover what a unit test over a fixture string cannot: what happens when
//! the feed host is gone, truncates a response, serves something that is not a
//! dataset, or publishes a document from a format this build has never seen.
//! Nothing here touches a live service - the "host" is a `wiremock` server on
//! localhost, so `cargo test --offline` passes with the network unplugged.
//!
//! The invariant every one of them defends is the same: a bad publish must
//! never replace a good cache, and must never stop the launcher applying
//! preferences with the data it already has.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ephervex_download::{
    CacheLayout, DomainAllowlist, DownloadManager, DownloaderConfig, HttpConfig, OperationReporter,
};
use ephervex_options::{
    CompatibilitySource, FeedError, FeedSource, OptionsCompatibility, OptionsDataset, OptionsFeed,
};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const GIST_PATH: &str = "/gists/84f4b9229c20bb0fcda74215aabe3e76";
const RAW_PATH: &str = "/raw/minecraft-options.json";
const FEED_FILE: &str = "minecraft-options.json";

fn manager() -> DownloadManager {
    DownloadManager::new(DownloaderConfig {
        http: HttpConfig::for_loopback_tests(),
        ..DownloaderConfig::default()
    })
    .expect("client builds")
}

fn feed(server: &MockServer, cache: &tempfile::TempDir) -> OptionsFeed {
    OptionsFeed::from_source(
        manager(),
        CacheLayout::new(cache.path()),
        FeedSource::gist("84f4b9229c20bb0fcda74215aabe3e76", FEED_FILE).through(&server.uri()),
    )
    .expect("feed builds")
    .with_allowlist(DomainAllowlist::for_loopback_tests("127.0.0.1"))
}

fn reporter() -> OperationReporter {
    OperationReporter::silent("minecraft-options-feed")
}

/// A published document one revision newer than the bundled baseline, so
/// adopting it is observable.
fn published() -> String {
    let bundled = OptionsDataset::bundled();
    published_revision(bundled.revision + 1)
}

fn published_revision(revision: u32) -> String {
    let bundled = OptionsDataset::bundled();
    include_str!("../data/minecraft-options.json").replacen(
        &format!("\"revision\": {}", bundled.revision),
        &format!("\"revision\": {revision}"),
        1,
    )
}

/// GitHub's Gist response with the document inline.
fn gist_with_content(body: &str) -> String {
    serde_json::json!({
        "files": {
            FEED_FILE: {
                "filename": FEED_FILE,
                "truncated": false,
                "content": body,
            }
        }
    })
    .to_string()
}

/// GitHub's Gist response for a file it cut short, pointing at the full body.
fn gist_truncated(server: &MockServer) -> String {
    serde_json::json!({
        "files": {
            FEED_FILE: {
                "filename": FEED_FILE,
                "truncated": true,
                "content": "{\"format_ver",
                "raw_url": format!("{}{RAW_PATH}", server.uri()),
            }
        }
    })
    .to_string()
}

async fn mount(server: &MockServer, route: &str, body: String) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_published_document_is_validated_then_cached_then_used() {
    let server = MockServer::start().await;
    mount(&server, GIST_PATH, gist_with_content(&published())).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let feed = feed(&server, &cache);

    let refreshed = feed
        .refresh(&reporter(), &CancellationToken::new())
        .await
        .expect("a valid publish is adopted");
    assert_eq!(refreshed.revision, OptionsDataset::bundled().revision + 1);

    // Cached, and usable without the network from here on.
    let cached = feed.cached().await.expect("the document was cached");
    assert_eq!(cached.revision, refreshed.revision);
    assert_eq!(
        OptionsCompatibility::resolve(Some(cached)).source(),
        CompatibilitySource::Feed
    );
}

#[tokio::test]
async fn a_truncated_gist_body_is_completed_from_the_raw_url_the_response_supplies() {
    // GitHub cuts long file bodies out of the Gist document and points at the
    // full one. Following that pointer is the difference between a feed that
    // works and a feed that silently stops updating once it grows.
    let server = MockServer::start().await;
    mount(&server, GIST_PATH, gist_truncated(&server)).await;
    mount(&server, RAW_PATH, published()).await;
    let cache = tempfile::tempdir().expect("temp dir");

    let refreshed = feed(&server, &cache)
        .refresh(&reporter(), &CancellationToken::new())
        .await
        .expect("the raw body completes the document");
    assert_eq!(refreshed.revision, OptionsDataset::bundled().revision + 1);
}

#[tokio::test]
async fn a_raw_url_pointing_off_the_allowlist_is_refused_rather_than_followed() {
    // `raw_url` arrives inside remote JSON, so it is untrusted input like any
    // other location a document names.
    let server = MockServer::start().await;
    mount(
        &server,
        GIST_PATH,
        serde_json::json!({
            "files": {
                FEED_FILE: {
                    "truncated": true,
                    "content": "{",
                    "raw_url": "https://evil.example/minecraft-options.json",
                }
            }
        })
        .to_string(),
    )
    .await;
    let cache = tempfile::tempdir().expect("temp dir");

    let error = feed(&server, &cache)
        .refresh(&reporter(), &CancellationToken::new())
        .await
        .expect_err("an unlisted host must not be fetched");
    assert!(
        error.to_string().contains("not permitted"),
        "unexpected failure: {error}"
    );
}

#[tokio::test]
async fn a_bad_publish_never_replaces_the_last_known_good_cache() {
    // The whole point of validating before caching. Four different ways of
    // being wrong, and after every one of them the good document is still
    // what a reader gets.
    let good = MockServer::start().await;
    mount(&good, GIST_PATH, gist_with_content(&published())).await;
    let cache = tempfile::tempdir().expect("temp dir");
    feed(&good, &cache)
        .refresh(&reporter(), &CancellationToken::new())
        .await
        .expect("the good publish is cached");

    let bundled = OptionsDataset::bundled().revision;
    let truncated = published().split_at(published().len() / 2).0.to_owned();
    let future_format = published().replacen("\"format_version\": 1", "\"format_version\": 99", 1);
    let dangling_schema = published().replacen(
        "\"canonical_minecraft_version\": \"26.2\"",
        "\"canonical_minecraft_version\": \"nothing-maps-this\"",
        1,
    );
    let executable_codec = published().replacen(
        "\"kind\": \"boolean\"",
        "\"kind\": \"eval\", \"expression\": \"value * 2\"",
        1,
    );
    assert_ne!(
        executable_codec,
        published(),
        "the hostile document must actually differ from the good one"
    );

    for (why, body) in [
        ("a truncated document", truncated),
        ("a format from the future", future_format),
        ("a semantically inconsistent document", dangling_schema),
        ("a codec this build does not implement", executable_codec),
        (
            "something that is not a dataset at all",
            "<html>503</html>".to_owned(),
        ),
    ] {
        let bad = MockServer::start().await;
        mount(&bad, GIST_PATH, gist_with_content(&body)).await;
        let feed = feed(&bad, &cache);

        assert!(
            feed.refresh(&reporter(), &CancellationToken::new())
                .await
                .is_err(),
            "{why} must be refused"
        );
        let cached = feed.cached().await.expect("the good document survives");
        assert_eq!(cached.revision, bundled + 1, "{why} replaced the cache");
    }
}

#[tokio::test]
async fn a_feed_host_that_cannot_be_reached_leaves_the_launcher_with_what_it_has() {
    // Offline with nothing cached is the fresh-installation case: the bundled
    // baseline answers every question, and the failure is observable rather
    // than fatal.
    let server = MockServer::start().await;
    let cache = tempfile::tempdir().expect("temp dir");
    let feed = feed(&server, &cache);

    assert!(
        feed.refresh(&reporter(), &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(feed.cached().await.is_none());

    let compatibility = OptionsCompatibility::resolve(feed.cached().await);
    assert_eq!(compatibility.source(), CompatibilitySource::Bundled);
    assert!(
        compatibility.schema_id("1.12.2").is_some(),
        "a fresh offline profile still understands every release this build shipped with"
    );
}

#[tokio::test]
async fn a_cached_document_survives_the_host_disappearing_afterwards() {
    let server = MockServer::start().await;
    mount(&server, GIST_PATH, gist_with_content(&published())).await;
    let cache = tempfile::tempdir().expect("temp dir");
    feed(&server, &cache)
        .refresh(&reporter(), &CancellationToken::new())
        .await
        .expect("cached while the host was up");
    drop(server);

    let offline = MockServer::start().await;
    let feed = feed(&offline, &cache);
    assert!(
        feed.refresh(&reporter(), &CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(
        OptionsCompatibility::resolve(feed.cached().await).source(),
        CompatibilitySource::Feed,
        "the last valid document keeps answering while the host is unreachable"
    );
}

#[tokio::test]
async fn a_gist_without_the_expected_file_is_refused_rather_than_guessed_at() {
    let server = MockServer::start().await;
    mount(
        &server,
        GIST_PATH,
        serde_json::json!({
            "files": {
                "something-else.json": {"truncated": false, "content": "{}"}
            }
        })
        .to_string(),
    )
    .await;
    let cache = tempfile::tempdir().expect("temp dir");

    let error = feed(&server, &cache)
        .refresh(&reporter(), &CancellationToken::new())
        .await
        .expect_err("no file, no dataset");
    assert!(error.to_string().contains(FEED_FILE), "{error}");
}

#[tokio::test]
async fn a_document_source_refreshes_from_its_exact_configured_host() {
    let server = MockServer::start().await;
    mount(&server, RAW_PATH, published()).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let feed = OptionsFeed::from_source(
        manager(),
        CacheLayout::new(cache.path()),
        FeedSource::Document {
            url: format!("{}{RAW_PATH}", server.uri()),
        },
    )
    .expect("document feed builds")
    .with_allowlist(DomainAllowlist::for_loopback_tests("127.0.0.1"));

    let refreshed = feed
        .refresh(&reporter(), &CancellationToken::new())
        .await
        .expect("the direct document is adopted");
    assert_eq!(refreshed.revision, OptionsDataset::bundled().revision + 1);
}

#[tokio::test]
async fn a_plaintext_document_source_is_refused_before_any_request() {
    let server = MockServer::start().await;
    let cache = tempfile::tempdir().expect("temp dir");
    let feed = OptionsFeed::from_source(
        manager(),
        CacheLayout::new(cache.path()),
        FeedSource::Document {
            url: format!("{}{RAW_PATH}", server.uri()),
        },
    )
    .expect("the URL is syntactically valid");

    let error = feed
        .refresh(&reporter(), &CancellationToken::new())
        .await
        .expect_err("production document sources require HTTPS");
    assert!(error.to_string().contains("not permitted"), "{error}");
    assert_eq!(server.received_requests().await.expect("requests").len(), 0);
}

async fn refresh_revision(
    cache: &tempfile::TempDir,
    revision: u32,
) -> Result<OptionsDataset, String> {
    let server = MockServer::start().await;
    mount(
        &server,
        GIST_PATH,
        gist_with_content(&published_revision(revision)),
    )
    .await;
    feed(&server, cache)
        .refresh(&reporter(), &CancellationToken::new())
        .await
        .map_err(|error| error.to_string())
}

#[tokio::test]
async fn a_remote_rollback_preserves_cached_revision_three() {
    let cache = tempfile::tempdir().expect("temp dir");
    refresh_revision(&cache, 3)
        .await
        .expect("revision 3 cached");
    let error = refresh_revision(&cache, 2)
        .await
        .expect_err("revision 2 is a rollback");
    assert!(error.contains("older than cached revision 3"), "{error}");
    let offline = MockServer::start().await;
    assert_eq!(
        feed(&offline, &cache)
            .cached()
            .await
            .expect("cached")
            .revision,
        3
    );
}

#[tokio::test]
async fn an_identical_remote_revision_is_a_safe_no_op() {
    let cache = tempfile::tempdir().expect("temp dir");
    refresh_revision(&cache, 3)
        .await
        .expect("revision 3 cached");
    let repeated = refresh_revision(&cache, 3)
        .await
        .expect("identical revision accepted");
    assert_eq!(repeated.revision, 3);
    let offline = MockServer::start().await;
    assert_eq!(
        feed(&offline, &cache)
            .cached()
            .await
            .expect("cached")
            .revision,
        3
    );
}

#[tokio::test]
async fn different_content_cannot_reuse_a_cached_revision() {
    let cache = tempfile::tempdir().expect("temp dir");
    refresh_revision(&cache, 3)
        .await
        .expect("revision 3 cached");
    let changed = published_revision(3).replacen(
        "\"category\": \"Controls\"",
        "\"category\": \"Gameplay\"",
        1,
    );
    assert_ne!(changed, published_revision(3));
    let server = MockServer::start().await;
    mount(&server, GIST_PATH, gist_with_content(&changed)).await;
    let error = feed(&server, &cache)
        .refresh(&reporter(), &CancellationToken::new())
        .await
        .expect_err("revision collision must be refused");
    assert!(error.to_string().contains("reused revision 3"), "{error}");
    assert_eq!(
        feed(&server, &cache)
            .cached()
            .await
            .expect("cached")
            .revision,
        3
    );
}

#[tokio::test]
async fn a_newer_remote_revision_replaces_the_cached_dataset() {
    let cache = tempfile::tempdir().expect("temp dir");
    refresh_revision(&cache, 3)
        .await
        .expect("revision 3 cached");
    assert_eq!(
        refresh_revision(&cache, 4)
            .await
            .expect("revision 4 adopted")
            .revision,
        4
    );
    let offline = MockServer::start().await;
    assert_eq!(
        feed(&offline, &cache)
            .cached()
            .await
            .expect("cached")
            .revision,
        4
    );
}

#[tokio::test]
async fn feeds_sharing_a_cache_each_adopt_only_the_response_of_their_own_source() {
    let slow = MockServer::start().await;
    let fast = MockServer::start().await;
    let slow_revision = OptionsDataset::bundled().revision + 1;
    let fast_revision = OptionsDataset::bundled().revision + 2;
    Mock::given(method("GET"))
        .and(path(RAW_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(published_revision(slow_revision))
                .set_delay(std::time::Duration::from_millis(150)),
        )
        .mount(&slow)
        .await;
    mount(&fast, RAW_PATH, published_revision(fast_revision)).await;

    let cache = tempfile::tempdir().expect("temp dir");
    let document_feed = |server: &MockServer| {
        OptionsFeed::from_source(
            manager(),
            CacheLayout::new(cache.path()),
            FeedSource::Document {
                url: format!("{}{RAW_PATH}", server.uri()),
            },
        )
        .expect("document feed builds")
        .with_allowlist(DomainAllowlist::for_loopback_tests("127.0.0.1"))
    };
    let (slow_feed, fast_feed) = (document_feed(&slow), document_feed(&fast));
    let cancel = CancellationToken::new();
    let reporter = reporter();

    let (slow_result, fast_result) = tokio::join!(slow_feed.refresh(&reporter, &cancel), async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        fast_feed.refresh(&reporter, &cancel).await
    });

    assert_eq!(
        fast_result
            .expect("the fast source's own response")
            .revision,
        fast_revision
    );
    // The slow feed finishes after the newer revision is cached, so refusing
    // it as a rollback is correct. Adopting the other source's response, or
    // failing because it vanished, is not.
    match slow_result {
        Ok(dataset) => assert_eq!(dataset.revision, slow_revision),
        Err(error) => assert!(
            matches!(error, FeedError::Rollback { .. }),
            "unexpected failure: {error}"
        ),
    }
    let staging_leftovers: Vec<_> = std::fs::read_dir(cache.path().join("metadata"))
        .expect("metadata store")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".part.") || name.contains(".fetch"))
        .collect();
    assert!(
        staging_leftovers.is_empty(),
        "staging outlived its refresh: {staging_leftovers:?}"
    );
}
