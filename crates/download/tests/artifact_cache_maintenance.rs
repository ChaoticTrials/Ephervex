//! Artifact-cache maintenance against real downloads and real locking.
//!
//! The unit tests beside the collector cover its bookkeeping. These cover the
//! two things only an integration test can: that a publication in progress is
//! safe from a collector, and that the maintenance lock is the kind of lock that
//! holds between processes.
//!
//! Offline throughout: every transfer goes to `wiremock` on loopback.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use ephervex_download::{
    ArtifactCache, ArtifactCacheError, CacheLayout, ContentStore, DownloadManager, DownloadRequest,
    DownloaderConfig, FileExtension, HttpConfig, MAINTENANCE_LOCK_FILE, OperationReporter,
    RetryPolicy,
};
use ephervex_model::{ArtifactCacheLimit, Hash, HashAlgorithm, MIB};
use sha1::{Digest as _, Sha1};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Over the smallest budget the settings model accepts, so a collection that
/// runs has something it is obliged to remove.
fn body() -> Vec<u8> {
    b"an artifact a collector must not take out from under a consumer\n".repeat(32_768)
}

fn sha1_of(bytes: &[u8]) -> Hash {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    Hash::new(HashAlgorithm::Sha1, &hex::encode(hasher.finalize())).expect("sha1")
}

fn manager() -> DownloadManager {
    DownloadManager::new(DownloaderConfig {
        http: HttpConfig::for_loopback_tests(),
        retry: RetryPolicy::none(),
        ..DownloaderConfig::default()
    })
    .expect("client builds")
}

#[tokio::test]
async fn a_publication_in_progress_is_neither_counted_nor_collectable() {
    let body = body();
    let hash = sha1_of(&body);
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/artifact.jar"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.clone())
                // Long enough that the assertions below all run while the
                // transfer is still writing its partial sibling. Correctness does
                // not depend on the duration: the lease is what excludes the
                // collector, and it is held across the whole transfer.
                .set_delay(Duration::from_millis(400)),
        )
        .mount(&server)
        .await;

    let temp = tempfile::tempdir().expect("tempdir");
    let layout = CacheLayout::new(temp.path().join("cache"));
    let cache = ArtifactCache::new(layout.clone());
    let extension = FileExtension::new("jar").expect("jar");
    let destination = layout.content(ContentStore::Downloads, &hash, Some(&extension));

    // Exactly what a core operation does: take the shared lease, then fetch.
    let lease = cache.lease().await.expect("lease");

    let transfer = {
        let manager = manager();
        let request = DownloadRequest::new(
            reqwest::Url::parse(&format!("{}/artifact.jar", server.uri())).expect("loopback url"),
            destination.clone(),
        )
        .verifying(hash.clone())
        .sized(body.len() as u64);
        tokio::spawn(async move {
            manager
                .download(
                    &request,
                    &OperationReporter::silent("artifact-cache-race"),
                    &CancellationToken::new(),
                )
                .await
        })
    };

    // While the transfer runs: nothing is publishable yet, so nothing is usage,
    // and a collector cannot start at all.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        cache.usage().await.expect("usage").bytes,
        0,
        "a partial download is not artifact usage"
    );
    assert!(matches!(
        cache.collect(ArtifactCacheLimit::Limited(MIB)).await,
        Err(ArtifactCacheError::Busy { .. })
    ));
    assert!(matches!(
        cache.clear().await,
        Err(ArtifactCacheError::Busy { .. })
    ));

    let outcome = transfer
        .await
        .expect("the transfer task completes")
        .expect("the artifact downloads and verifies");
    assert_eq!(outcome.path, destination);
    assert_eq!(outcome.bytes, body.len() as u64);
    assert_eq!(
        std::fs::read(&destination).expect("published artifact"),
        body,
        "publication is unaffected by maintenance having been attempted"
    );

    // The operation is over. Now, and only now, the budget applies.
    drop(lease);
    let report = cache
        .collect(ArtifactCacheLimit::Limited(MIB))
        .await
        .expect("collection runs once the lease is released");
    assert_eq!(report.removed_entries, 1);
    assert!(!destination.exists());
}

#[tokio::test]
async fn the_maintenance_lock_contends_between_independent_descriptors() {
    // This is the property that makes the lock cross-process rather than
    // merely cross-task: two separately opened descriptors on the lock file
    // contend with each other, which is exactly the situation two Ephervex
    // processes are in. Nothing here depends on timing.
    let temp = tempfile::tempdir().expect("tempdir");
    let cache = ArtifactCache::new(CacheLayout::new(temp.path().join("cache")));

    let first = cache.lease().await.expect("one operation's lease");
    let second = ArtifactCache::new(CacheLayout::new(temp.path().join("cache")))
        .lease()
        .await
        .expect("another holder's lease, taken through its own descriptor");

    assert!(matches!(
        cache.clear().await,
        Err(ArtifactCacheError::Busy { .. })
    ));
    drop(first);
    assert!(
        matches!(cache.clear().await, Err(ArtifactCacheError::Busy { .. })),
        "one remaining holder is still enough to exclude maintenance"
    );
    drop(second);
    assert!(cache.clear().await.is_ok());

    assert_eq!(
        cache
            .lock_path()
            .file_name()
            .and_then(std::ffi::OsStr::to_str),
        Some(MAINTENANCE_LOCK_FILE),
        "the lock has a fixed launcher-owned name, never a derived one"
    );
    assert_eq!(
        cache.lock_path().parent(),
        Some(cache.layout().root()),
        "the lock lives at the cache root, outside every store the collector sweeps"
    );
}
