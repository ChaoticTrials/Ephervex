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
    CompatibilitySource, DEFAULT_FEED_URL, FeedError, FeedSource, OPTIONS_FEED_CACHE_KEY,
    OptionsCompatibility, OptionsDataset, OptionsFeed,
};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DOC_PATH: &str = "/assets/minecraft-options.json";

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
        FeedSource::document(&format!("{}{DOC_PATH}", server.uri())),
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

async fn mount(server: &MockServer, route: &str, body: impl Into<String>) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_string(body.into()))
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_published_document_is_validated_then_cached_then_used() {
    let server = MockServer::start().await;
    mount(&server, DOC_PATH, &published()).await;
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
async fn a_bad_publish_never_replaces_the_last_known_good_cache() {
    // The whole point of validating before caching. Four different ways of
    // being wrong, and after every one of them the good document is still
    // what a reader gets.
    let good = MockServer::start().await;
    mount(&good, DOC_PATH, &published()).await;
    let cache = tempfile::tempdir().expect("temp dir");
    feed(&good, &cache)
        .refresh(&reporter(), &CancellationToken::new())
        .await
        .expect("the good publish is cached");

    let bundled = OptionsDataset::bundled().revision;
    let truncated = published().split_at(published().len() / 2).0.to_owned();
    let future_format = published().replacen("\"format_version\": 1", "\"format_version\": 99", 1);
    let dangling_schema = published().replacen(
        &format!(
            "\"canonical_minecraft_version\": \"{}\"",
            OptionsDataset::bundled().canonical_minecraft_version
        ),
        "\"canonical_minecraft_version\": \"nothing-maps-this\"",
        1,
    );
    assert_ne!(
        dangling_schema,
        published(),
        "the hostile document must actually differ from the good one"
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
        mount(&bad, DOC_PATH, &body).await;
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
async fn a_site_error_response_never_replaces_the_cached_document() {
    // The documentation site is a static host: when it is down or the file is
    // missing it answers with an error page, not with a dataset.
    let good = MockServer::start().await;
    mount(&good, DOC_PATH, published()).await;
    let cache = tempfile::tempdir().expect("temp dir");
    feed(&good, &cache)
        .refresh(&reporter(), &CancellationToken::new())
        .await
        .expect("the good publish is cached");

    for status in [404, 500, 503] {
        let failing = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(DOC_PATH))
            .respond_with(ResponseTemplate::new(status).set_body_string("<html>error</html>"))
            .mount(&failing)
            .await;
        let feed = feed(&failing, &cache);

        assert!(
            feed.refresh(&reporter(), &CancellationToken::new())
                .await
                .is_err(),
            "HTTP {status} must not count as a publish"
        );
        let cached = feed.cached().await.expect("the good document survives");
        assert_eq!(
            cached.revision,
            OptionsDataset::bundled().revision + 1,
            "HTTP {status} replaced the cache"
        );
    }
}

#[tokio::test]
async fn the_default_feed_reads_only_the_documentation_site() {
    let cache = tempfile::tempdir().expect("temp dir");
    let feed = OptionsFeed::new(manager(), CacheLayout::new(cache.path())).expect("feed builds");

    assert_eq!(feed.source(), &FeedSource::default());
    assert_eq!(
        feed.source().entry_url().expect("valid url").as_str(),
        DEFAULT_FEED_URL
    );
}

#[tokio::test]
async fn a_cached_document_survives_the_host_disappearing_afterwards() {
    let server = MockServer::start().await;
    mount(&server, DOC_PATH, &published()).await;
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
async fn a_plaintext_document_source_is_refused_before_any_request() {
    let server = MockServer::start().await;
    let cache = tempfile::tempdir().expect("temp dir");
    let feed = OptionsFeed::from_source(
        manager(),
        CacheLayout::new(cache.path()),
        FeedSource::document(&format!("{}{DOC_PATH}", server.uri())),
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
    mount(&server, DOC_PATH, &published_revision(revision)).await;
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
    mount(&server, DOC_PATH, &changed).await;
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

/// Writes a cache envelope whose payload is not a dataset this build accepts.
async fn cache_unusable_document(cache: &tempfile::TempDir) {
    let layout = CacheLayout::new(cache.path());
    let server = MockServer::start().await;
    let unusable = serde_json::value::RawValue::from_string("{\"format_version\":1}".to_owned())
        .expect("valid json");
    let key = ephervex_download::MetadataKey::new(OPTIONS_FEED_CACHE_KEY).expect("key");
    ephervex_download::MetadataCache::new(layout)
        .store(
            &key,
            &ephervex_download::Url::parse(&server.uri()).expect("url"),
            unusable,
        )
        .await
        .expect("stored");
}

#[tokio::test]
async fn an_unusable_cached_document_is_discarded_when_read() {
    let cache = tempfile::tempdir().expect("temp dir");
    cache_unusable_document(&cache).await;
    let server = MockServer::start().await;
    let feed = feed(&server, &cache);

    assert!(feed.cached().await.is_none());

    let layout = CacheLayout::new(cache.path());
    let key = ephervex_download::MetadataKey::new(OPTIONS_FEED_CACHE_KEY).expect("key");
    assert!(
        ephervex_download::MetadataCache::new(layout)
            .peek(&key)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn an_unusable_cached_document_is_discarded_when_checked_for_freshness() {
    let cache = tempfile::tempdir().expect("temp dir");
    cache_unusable_document(&cache).await;
    let server = MockServer::start().await;
    let feed = feed(&server, &cache);

    assert!(!feed.is_fresh().await);

    let layout = CacheLayout::new(cache.path());
    let key = ephervex_download::MetadataKey::new(OPTIONS_FEED_CACHE_KEY).expect("key");
    assert!(
        ephervex_download::MetadataCache::new(layout)
            .peek(&key)
            .await
            .is_none()
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
        .and(path(DOC_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(published_revision(slow_revision))
                .set_delay(std::time::Duration::from_millis(150)),
        )
        .mount(&slow)
        .await;
    mount(&fast, DOC_PATH, published_revision(fast_revision)).await;

    let cache = tempfile::tempdir().expect("temp dir");
    let document_feed = |server: &MockServer| {
        OptionsFeed::from_source(
            manager(),
            CacheLayout::new(cache.path()),
            FeedSource::document(&format!("{}{DOC_PATH}", server.uri())),
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

/// Another process publishes between this refresh's download and its
/// publication: the refresh must compare against that publication and refuse
/// to replace it.
#[cfg(unix)]
#[tokio::test]
async fn a_refresh_never_replaces_a_newer_revision_published_by_another_process_meanwhile() {
    use ephervex_download::{MetadataCache, MetadataKey};
    use rustix::fs::{FlockOperation, flock};

    let server = MockServer::start().await;
    let downloaded = OptionsDataset::bundled().revision + 1;
    let published_elsewhere = downloaded + 1;
    mount(&server, DOC_PATH, &published_revision(downloaded)).await;
    let cache = tempfile::tempdir().expect("temp dir");
    let refreshing = feed(&server, &cache);

    // The entry's lock is held the way another process holds it while it
    // publishes.
    let metadata = cache.path().join("metadata");
    std::fs::create_dir_all(&metadata).expect("metadata dir");
    let other_process = std::fs::File::create(
        metadata.join(format!("{}.lock", ephervex_options::OPTIONS_FEED_CACHE_KEY)),
    )
    .expect("lock file");
    flock(&other_process, FlockOperation::LockExclusive).expect("lock");

    let refresh = tokio::spawn(async move {
        refreshing
            .refresh(&reporter(), &CancellationToken::new())
            .await
    });
    while server
        .received_requests()
        .await
        .expect("requests")
        .is_empty()
    {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !refresh.is_finished(),
        "published without taking the cache entry's lock"
    );

    let key = MetadataKey::new(ephervex_options::OPTIONS_FEED_CACHE_KEY).expect("key");
    let url = reqwest::Url::parse(&server.uri()).expect("url");
    MetadataCache::new(CacheLayout::new(cache.path()))
        .store(
            &key,
            &url,
            serde_json::value::RawValue::from_string(published_revision(published_elsewhere))
                .expect("json"),
        )
        .await
        .expect("stores");
    drop(other_process);

    let error = refresh
        .await
        .expect("task")
        .expect_err("the newer revision must survive");
    assert!(
        matches!(
            error,
            FeedError::Rollback { incoming, cached }
                if incoming == downloaded && cached == published_elsewhere
        ),
        "unexpected failure: {error}"
    );
    let cached = feed(&server, &cache).cached().await.expect("cached");
    assert_eq!(cached.revision, published_elsewhere);
}
