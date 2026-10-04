//! Integration tests for the download manager against a local HTTP server
//! (the download subsystem regression coverage).
//!
//! Everything here runs against `wiremock` on loopback. No test contacts
//! Mojang, Modrinth, CurseForge, Forge, NeoForge, or Microsoft, so ordinary CI
//! never depends on a live service and `cargo test --workspace --offline`
//! passes once dependencies are vendored.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ephervex_download::{
    ArtifactSource, BatchDownloadError, CacheLayout, ChannelSink, ContentStore, DownloadError,
    DownloadManager, DownloadRequest, DownloaderConfig, FileExtension, HttpConfig,
    OperationReporter, ProgressSink, RetryPolicy, UrlPolicyError,
};
use ephervex_model::{Hash, HashAlgorithm, OperationStatus};
use sha1::{Digest, Sha1};
use sha2::Sha512;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BODY: &[u8] = b"a fixture artifact standing in for a mod jar";

fn sha1_of(bytes: &[u8]) -> Hash {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    Hash::new(HashAlgorithm::Sha1, &hex::encode(hasher.finalize())).expect("computed sha1 is valid")
}

fn manager(retry: RetryPolicy) -> DownloadManager {
    manager_with(retry, 4)
}

fn manager_with(retry: RetryPolicy, concurrency: usize) -> DownloadManager {
    DownloadManager::new(DownloaderConfig {
        concurrency,
        retry,
        http: HttpConfig::for_loopback_tests(),
        allow_resume: true,
        max_bytes: None,
    })
    .expect("client builds")
}

/// Requests always name their own destination; the URL never chooses it.
fn request(server: &MockServer, route: &str, destination: &std::path::Path) -> DownloadRequest {
    let url = reqwest::Url::parse(&format!("{}{route}", server.uri())).expect("loopback url");
    DownloadRequest::new(url, destination)
}

fn no_partial_siblings(destination: &std::path::Path) -> bool {
    let Some(parent) = destination.parent() else {
        return true;
    };
    let Some(name) = destination.file_name().and_then(std::ffi::OsStr::to_str) else {
        return true;
    };
    // Matched on the marker rather than on the destination's name: a temporary
    // name is bounded, so it does not necessarily begin with the name it
    // belongs to.
    let _ = name;
    std::fs::read_dir(parent).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().contains(".part."))
    })
}

fn sha512_of(bytes: &[u8]) -> Hash {
    let mut hasher = Sha512::new();
    hasher.update(bytes);
    Hash::new(HashAlgorithm::Sha512, &hex::encode(hasher.finalize()))
        .expect("computed sha512 is valid")
}

#[tokio::test]
async fn a_successful_download_lands_at_the_destination_with_its_bytes_intact() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/artifact.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("store").join("artifact.jar");
    let outcome = manager(RetryPolicy::none())
        .download(
            &request(&server, "/artifact.jar", &destination).verifying(sha1_of(BODY)),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect("download succeeds");

    assert_eq!(outcome.source, ArtifactSource::Network);
    assert_eq!(outcome.bytes, BODY.len() as u64);
    assert_eq!(std::fs::read(&destination).expect("artifact exists"), BODY);
    assert!(
        no_partial_siblings(&destination),
        "the.part file must be gone once the artifact is finalized"
    );
}

#[tokio::test]
async fn a_redirect_is_followed_to_the_final_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/redirect"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{}/final", server.uri())),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/final"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    manager(RetryPolicy::none())
        .download(
            &request(&server, "/redirect", &destination).verifying(sha1_of(BODY)),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect("redirect is followed");

    assert_eq!(std::fs::read(&destination).expect("artifact exists"), BODY);
}

#[tokio::test]
async fn a_redirect_loop_is_bounded_rather_than_followed_forever() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/loop"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{}/loop", server.uri())),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    let error = manager(RetryPolicy::none())
        .download(
            &request(&server, "/loop", &destination),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("a redirect loop must terminate");

    assert!(matches!(error, DownloadError::Transport { .. }));
    assert!(!destination.exists());
}

#[tokio::test]
async fn a_transient_server_error_is_retried_until_it_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/artifact.jar"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/artifact.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    let retry = RetryPolicy {
        max_attempts: 4,
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(20),
        multiplier: 2,
    };
    manager(retry)
        .download(
            &request(&server, "/artifact.jar", &destination).verifying(sha1_of(BODY)),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect("the third attempt succeeds");

    assert_eq!(std::fs::read(&destination).expect("artifact exists"), BODY);
    let requests = server.received_requests().await.expect("recording enabled");
    assert_eq!(requests.len(), 3, "two failures then one success");
}

#[tokio::test]
async fn a_permanently_transient_failure_reports_the_exhausted_retry_budget() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/artifact.jar"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    let retry = RetryPolicy {
        max_attempts: 3,
        initial_backoff: Duration::from_millis(5),
        max_backoff: Duration::from_millis(10),
        multiplier: 2,
    };
    let error = manager(retry)
        .download(
            &request(&server, "/artifact.jar", &destination),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("503 forever must eventually fail");

    match error {
        DownloadError::RetriesExhausted { attempts, .. } => assert_eq!(attempts, 3),
        other => panic!("expected an exhausted retry budget, got {other:?}"),
    }
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("recording enabled")
            .len(),
        3
    );
}

#[tokio::test]
async fn a_not_found_is_reported_immediately_without_retrying() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing.jar"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("missing.jar");
    let retry = RetryPolicy {
        max_attempts: 5,
        initial_backoff: Duration::from_millis(5),
        max_backoff: Duration::from_millis(10),
        multiplier: 2,
    };
    let error = manager(retry)
        .download(
            &request(&server, "/missing.jar", &destination),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("404 must fail");

    assert!(matches!(
        error,
        DownloadError::HttpStatus { status: 404, .. }
    ));
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("recording enabled")
            .len(),
        1,
        "a 404 will never become a 200; retrying it is pure noise"
    );
}

#[tokio::test]
async fn a_hash_mismatch_deletes_the_artifact_and_fails() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/artifact.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"tampered payload".to_vec()))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    let retry = RetryPolicy {
        max_attempts: 3,
        initial_backoff: Duration::from_millis(5),
        max_backoff: Duration::from_millis(10),
        multiplier: 2,
    };
    let error = manager(retry)
        .download(
            &request(&server, "/artifact.jar", &destination).verifying(sha1_of(BODY)),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the digest does not match");

    match error {
        DownloadError::HashMismatch {
            algorithm,
            expected,
            actual,
            ..
        } => {
            assert_eq!(algorithm, HashAlgorithm::Sha1);
            assert_ne!(expected, actual);
        }
        other => panic!("expected a hash mismatch, got {other:?}"),
    }
    assert!(!destination.exists(), "the bad artifact must be deleted");
    assert!(no_partial_siblings(&destination));
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("recording enabled")
            .len(),
        1,
        "wrong bytes are not a transient failure"
    );
}

#[tokio::test]
async fn a_partial_download_never_reaches_the_final_path() {
    let server = MockServer::start().await;
    // A body that is well-formed HTTP but is not the artifact that was asked
    // for: the transfer completes, verification rejects it, and the atomic
    // finalization must therefore never have happened.
    Mock::given(method("GET"))
        .and(path("/artifact.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"half".to_vec()))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("store").join("artifact.jar");
    let error = manager(RetryPolicy::none())
        .download(
            &request(&server, "/artifact.jar", &destination)
                .verifying(sha1_of(BODY))
                .sized(BODY.len() as u64),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("a short body must not be accepted");

    assert!(matches!(error, DownloadError::SizeMismatch { .. }));
    assert!(
        !destination.exists(),
        "presence at a cache path is proof of a verified artifact and must stay that way"
    );
    assert!(no_partial_siblings(&destination));
    // The destination directory was created, so the absence above is a real
    // absence and not a missing parent.
    assert!(destination.parent().expect("has a parent").is_dir());
}

#[tokio::test]
async fn a_size_mismatch_is_rejected_even_when_no_hash_was_supplied() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/artifact.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    let error = manager(RetryPolicy::none())
        .download(
            &request(&server, "/artifact.jar", &destination).sized(4),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the declared size does not match");

    assert!(matches!(
        error,
        DownloadError::SizeMismatch { expected: 4, .. }
    ));
    assert!(!destination.exists());
}

#[tokio::test]
async fn a_body_larger_than_the_requests_ceiling_is_abandoned_rather_than_written() {
    // The shape of an untrusted document nobody published a size or a hash for:
    // a provider metadata response. Without a ceiling it streams until the disk
    // is full, so the request carries its own.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/manifest.json"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("manifest.json");
    let error = manager(RetryPolicy::none())
        .download(
            &request(&server, "/manifest.json", &destination).bounded_by(8),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the body overruns the ceiling");

    assert!(matches!(
        error,
        DownloadError::BodyTooLarge { limit: 8, .. }
    ));
    assert!(!destination.exists());
    assert!(
        no_partial_siblings(&destination),
        "an abandoned oversize body leaves no staging file"
    );
}

#[tokio::test]
async fn a_request_ceiling_cannot_raise_the_managers_own_limit() {
    // A manager sized for artifacts and a request sized for a document meet at
    // the tighter of the two, never the looser.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/artifact.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let manager = DownloadManager::new(DownloaderConfig {
        concurrency: 1,
        retry: RetryPolicy::none(),
        http: HttpConfig::for_loopback_tests(),
        allow_resume: true,
        max_bytes: Some(8),
    })
    .expect("client builds");

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    let error = manager
        .download(
            &request(&server, "/artifact.jar", &destination).bounded_by(u64::MAX),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the manager ceiling still applies");

    assert!(matches!(
        error,
        DownloadError::BodyTooLarge { limit: 8, .. }
    ));
    assert!(!destination.exists());
}

#[tokio::test]
async fn a_body_within_the_ceiling_is_downloaded_normally() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/manifest.json"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("manifest.json");
    manager(RetryPolicy::none())
        .download(
            &request(&server, "/manifest.json", &destination).bounded_by(BODY.len() as u64),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect("a body exactly at the ceiling is not over it");

    assert_eq!(std::fs::read(&destination).expect("body"), BODY);
}

#[tokio::test]
async fn a_canceled_download_leaves_no_file_behind() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow.jar"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(BODY)
                .set_delay(Duration::from_secs(3)),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("slow.jar");
    let cancel = CancellationToken::new();
    let reporter = OperationReporter::silent("download");

    let canceller = cancel.clone();
    let canceling = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        canceller.cancel();
    });

    let error = manager(RetryPolicy::none())
        .download(
            &request(&server, "/slow.jar", &destination),
            &reporter,
            &cancel,
        )
        .await
        .expect_err("cancellation must abort the download");
    canceling.await.expect("canceller task");

    assert!(matches!(error, DownloadError::Canceled { .. }));
    assert!(error.is_cancellation());
    assert!(!destination.exists());
    assert!(no_partial_siblings(&destination));
    assert_eq!(
        reporter.snapshot().status,
        OperationStatus::Canceled,
        "cancellation is a distinct outcome from failure"
    );
}

#[tokio::test]
async fn an_already_cached_artifact_is_not_fetched_again() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/artifact.jar"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    std::fs::write(&destination, BODY).expect("seed the cache");

    let outcome = manager(RetryPolicy::none())
        .download(
            &request(&server, "/artifact.jar", &destination).verifying(sha1_of(BODY)),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect("a cached artifact needs no network");

    assert_eq!(outcome.source, ArtifactSource::AlreadyCached);
    assert_eq!(outcome.bytes, BODY.len() as u64);
    assert!(
        server
            .received_requests()
            .await
            .expect("recording enabled")
            .is_empty(),
        "a cached artifact must not produce a request"
    );
}

#[tokio::test]
async fn a_corrupt_cache_entry_is_reverified_and_replaced() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/artifact.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    let wrong = vec![b'x'; BODY.len()];
    std::fs::write(&destination, wrong).expect("seed corrupt cache");

    let outcome = manager(RetryPolicy::none())
        .download(
            &request(&server, "/artifact.jar", &destination)
                .verifying(sha1_of(BODY))
                .sized(BODY.len() as u64),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect("the corrupt cache entry is replaced");

    assert_eq!(outcome.source, ArtifactSource::Network);
    assert_eq!(std::fs::read(&destination).expect("artifact"), BODY);
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("recording enabled")
            .len(),
        1
    );
}

#[tokio::test]
async fn the_cache_probe_answers_from_disk_alone_and_agrees_with_a_download() {
    // The probe exists so a caller can find out whether a fetch is going to
    // happen *before* paying whatever a fetch costs it - reading a provider
    // credential, most of all. It must therefore reach nobody, and it must
    // apply the same revalidation a download would.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/artifact.jar"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    let manager = manager(RetryPolicy::none());
    let probe = request(&server, "/artifact.jar", &destination)
        .verifying(sha1_of(BODY))
        .sized(BODY.len() as u64);

    assert!(
        manager
            .cached(&probe)
            .await
            .expect("a probe of nothing")
            .is_none(),
        "an absent artifact is not a cache hit"
    );

    std::fs::write(&destination, BODY).expect("seed the cache");
    let hit = manager
        .cached(&probe)
        .await
        .expect("a probe of a valid entry")
        .expect("the entry matches its request");
    assert_eq!(hit.source, ArtifactSource::AlreadyCached);
    assert_eq!(hit.bytes, BODY.len() as u64);
    assert_eq!(hit.path, destination);

    assert!(
        server
            .received_requests()
            .await
            .expect("recording enabled")
            .is_empty(),
        "the probe contacted the artifact's source"
    );
}

#[tokio::test]
async fn the_cache_probe_discards_an_entry_that_does_not_match_its_request() {
    // The same rule a download applies: presence at a cache path only means
    // something because a mismatching file is removed rather than trusted.
    let server = MockServer::start().await;
    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    std::fs::write(&destination, vec![b'x'; BODY.len()]).expect("seed a corrupt entry");

    let probe = request(&server, "/artifact.jar", &destination)
        .verifying(sha1_of(BODY))
        .sized(BODY.len() as u64);
    assert!(
        manager(RetryPolicy::none())
            .cached(&probe)
            .await
            .expect("a probe of a corrupt entry")
            .is_none()
    );
    assert!(
        !destination.exists(),
        "a cache entry that does not match its request must not survive the probe"
    );
}

#[tokio::test]
async fn a_credential_confines_its_own_redirects_without_confining_anything_else() {
    // Redirect containment travels with the credential rather than with the
    // manager, which is what lets one batch fetch an authenticated artifact and
    // an unrelated provider's artifact through the same downloader: the
    // authenticated request is stopped at its allowlist, and the open one still
    // follows the redirect its own CDN answers with.
    let server = MockServer::start().await;
    let elsewhere = format!("http://localhost:{}/elsewhere.jar", server.address().port());
    for route in ["/authenticated.jar", "/open.jar"] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(302).insert_header("location", elsewhere.as_str()))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/elsewhere.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    // `127.0.0.1` and `localhost` are the same machine and different hosts,
    // which is exactly the shape of the leak this guards against.
    let credential = Arc::new(
        ephervex_download::AuthenticatedSource::new(
            reqwest::header::HeaderName::from_static("x-api-key"),
            ephervex_model::SecretString::new("test-key-do-not-use"),
            ephervex_download::DomainAllowlist::for_loopback_tests("127.0.0.1"),
            &HttpConfig::for_loopback_tests(),
        )
        .expect("a credential builds its containment client"),
    );

    let dir = TempDir::new().expect("temp dir");
    let manager = manager(RetryPolicy::none());
    let reporter = OperationReporter::silent("download");
    let cancel = CancellationToken::new();

    let confined = dir.path().join("authenticated.jar");
    let error = manager
        .download(
            &request(&server, "/authenticated.jar", &confined)
                .verifying(sha1_of(BODY))
                .authenticated_by(Arc::clone(&credential)),
            &reporter,
            &cancel,
        )
        .await
        .expect_err("a redirect off the permitted hosts is refused");
    assert!(
        matches!(error, DownloadError::HttpStatus { status: 302, .. }),
        "{error:?}"
    );
    assert!(!confined.exists());

    let open = dir.path().join("open.jar");
    let outcome = manager
        .download(
            &request(&server, "/open.jar", &open).verifying(sha1_of(BODY)),
            &reporter,
            &cancel,
        )
        .await
        .expect("an unauthenticated download still follows an ordinary redirect");
    assert_eq!(outcome.source, ArtifactSource::Network);
    assert_eq!(std::fs::read(&open).expect("artifact"), BODY);

    let recorded = server.received_requests().await.expect("recording enabled");
    let reached_elsewhere: Vec<_> = recorded
        .iter()
        .filter(|request| request.url.path() == "/elsewhere.jar")
        .collect();
    assert_eq!(
        reached_elsewhere.len(),
        1,
        "only the unauthenticated download may leave the origin"
    );
    assert!(
        !reached_elsewhere[0].headers.contains_key("x-api-key"),
        "the credential reached a host outside its allowlist"
    );
}

#[tokio::test]
async fn every_request_identifies_ephervex_and_its_project_url() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/artifact.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    manager(RetryPolicy::none())
        .download(
            &request(&server, "/artifact.jar", &destination),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect("download succeeds");

    let requests = server.received_requests().await.expect("recording enabled");
    let agent = requests[0]
        .headers
        .get("user-agent")
        .expect("user-agent is always sent")
        .to_str()
        .expect("user-agent is ascii");
    assert_eq!(agent, ephervex_model::user_agent());
    assert!(agent.starts_with("Ephervex/"));
    assert!(agent.contains("github.com/ChaoticTrials/Ephervex"));
}

#[tokio::test]
async fn a_plaintext_http_url_is_refused_by_the_default_configuration() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/artifact.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    // The production configuration, not the loopback test one.
    let production = DownloadManager::new(DownloaderConfig::default()).expect("client builds");
    let error = production
        .download(
            &request(&server, "/artifact.jar", &destination),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("plaintext must be refused");

    assert!(matches!(
        error,
        DownloadError::Policy(UrlPolicyError::SchemeNotPermitted { .. })
    ));
    assert!(
        server
            .received_requests()
            .await
            .expect("recording enabled")
            .is_empty(),
        "the policy must reject before any socket is opened"
    );
}

#[tokio::test]
async fn a_batch_downloads_every_file_and_reports_one_operation() {
    let server = MockServer::start().await;
    for index in 0..5 {
        Mock::given(method("GET"))
            .and(path(format!("/file-{index}.jar")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
            .mount(&server)
            .await;
    }

    let dir = TempDir::new().expect("temp dir");
    let requests: Vec<DownloadRequest> = (0..5)
        .map(|index| {
            request(
                &server,
                &format!("/file-{index}.jar"),
                &dir.path().join(format!("file-{index}.jar")),
            )
            .verifying(sha1_of(BODY))
            .sized(BODY.len() as u64)
            .labeled(format!("file-{index}.jar"))
        })
        .collect();

    let (sink, mut events) = ChannelSink::new();
    let sink: Arc<dyn ProgressSink> = sink;
    let reporter = OperationReporter::new("download-batch", true, sink);
    let outcomes = manager(RetryPolicy::none())
        .download_all(&requests, &reporter, &CancellationToken::new())
        .await
        .expect("the batch succeeds");

    assert_eq!(outcomes.len(), 5);
    for (index, outcome) in outcomes.iter().enumerate() {
        assert_eq!(outcome.path, dir.path().join(format!("file-{index}.jar")));
        assert_eq!(std::fs::read(&outcome.path).expect("artifact exists"), BODY);
    }

    let mut snapshots = Vec::new();
    while let Ok(operation) = events.try_recv() {
        snapshots.push(operation);
    }
    let last = snapshots.last().expect("at least one snapshot");
    assert_eq!(last.status, OperationStatus::Completed);
    assert_eq!(last.progress.total, Some(5 * BODY.len() as u64));
    assert_eq!(last.progress.fraction(), Some(1.0));
    assert!(
        snapshots.iter().all(|snapshot| snapshot.id == last.id),
        "the whole batch is one operation, not five"
    );
}

/// A loopback server that holds every response until the test releases it.
///
/// `wiremock` can only delay a response by a fixed duration, which makes the
/// order transfers finish in likely rather than known.
struct GatedServer {
    base: String,
    /// Each request's path as it arrives, with the switch that lets it finish.
    arrivals: tokio::sync::mpsc::UnboundedReceiver<(String, tokio::sync::oneshot::Sender<()>)>,
}

impl GatedServer {
    async fn spawn(body: &'static [u8]) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("local address"));
        let (arrived, arrivals) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let arrived = arrived.clone();
                tokio::spawn(async move {
                    let head = read_request_head(&mut stream).await;
                    let route = head
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    let (release, released) = tokio::sync::oneshot::channel();
                    if arrived.send((route, release)).is_err() || released.await.is_err() {
                        return;
                    }
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(header.as_bytes()).await;
                    let _ = stream.write_all(body).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self { base, arrivals }
    }

    fn url(&self, route: &str) -> reqwest::Url {
        reqwest::Url::parse(&format!("{}{route}", self.base)).expect("loopback url")
    }
}

async fn read_request_head(stream: &mut tokio::net::TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte).await {
            Ok(0) | Err(_) => break,
            Ok(_) => head.push(byte[0]),
        }
    }
    String::from_utf8_lossy(&head).into_owned()
}

/// The name beside a batch's count has to be the file that count refers to.
///
/// It was announced from inside each download's own future, and
/// `FuturesUnordered` polls every future it holds before any of them finishes,
/// so all of the names were published in one burst before the first byte
/// arrived. The count then climbed for the length of the batch beside a file
/// name that had been chosen at the start and never moved.
#[tokio::test]
async fn the_file_named_beside_a_batch_count_advances_with_it() {
    // Deliberately not request order, so a name that follows the order the
    // batch was submitted in cannot pass.
    const RELEASE_ORDER: [usize; 8] = [5, 2, 7, 0, 3, 6, 1, 4];

    let mut server = GatedServer::spawn(BODY).await;
    let dir = TempDir::new().expect("temp dir");
    let requests: Vec<DownloadRequest> = (0..8)
        .map(|index| {
            let name = format!("file-{index}.jar");
            DownloadRequest::new(server.url(&format!("/{name}")), dir.path().join(&name))
                .verifying(sha1_of(BODY))
                .sized(BODY.len() as u64)
                .labeled(name)
        })
        .collect();

    let (sink, mut events) = ChannelSink::new();
    let sink: Arc<dyn ProgressSink> = sink;
    let reporter = OperationReporter::new("download-batch", true, sink);
    let manager = manager_with(RetryPolicy::none(), 8);
    let cancel = CancellationToken::new();
    let batch = manager.download_all(&requests, &reporter, &cancel);

    let release_in_order = async {
        let mut gates = std::collections::HashMap::new();
        while gates.len() < RELEASE_ORDER.len() {
            let (route, release) = server.arrivals.recv().await.expect("server is running");
            gates.insert(route, release);
        }
        // Every request is in flight before any of them may finish.
        let before = reporter.snapshot();
        assert_eq!(before.items.current, 0);
        assert_eq!(before.current_step, None);

        for (landed, index) in (1..).zip(RELEASE_ORDER) {
            let name = format!("file-{index}.jar");
            gates
                .remove(&format!("/{name}"))
                .expect("every file was requested")
                .send(())
                .expect("the request is still waiting");
            let snapshot = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let snapshot = reporter.snapshot();
                    if snapshot.items.current >= landed {
                        break snapshot;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("the released file is counted");
            assert_eq!(
                snapshot.items.current, landed,
                "only one file was allowed to finish"
            );
            assert_eq!(
                snapshot.current_step.as_deref(),
                Some(name.as_str()),
                "the name beside `{landed} of 8` is not the file that count refers to"
            );
        }
    };
    let (outcome, ()) = tokio::join!(batch, release_in_order);
    outcome.expect("the batch succeeds");

    // What was published is a throttled sample of the same states, and has to
    // agree with them.
    let mut last = None;
    while let Ok(operation) = events.try_recv() {
        if let Some(named) = operation.current_step.as_deref() {
            let landed = usize::try_from(operation.items.current).expect("a count of eight files");
            assert!(
                landed > 0,
                "`{named}` was named before anything had finished downloading"
            );
            assert_eq!(
                named,
                format!("file-{}.jar", RELEASE_ORDER[landed - 1]),
                "the name published beside `{landed} of 8` is not the file that count refers to"
            );
        }
        last = Some(operation);
    }
    let last = last.expect("at least one snapshot");
    assert_eq!(last.status, OperationStatus::Completed);
    assert_eq!(last.items.current, 8);
    assert_eq!(last.current_step.as_deref(), Some("file-4.jar"));
}

/// A batch of cached artifacts must not open all of them at once.
///
/// The cache probe runs before a transfer permit is taken, so that a cached
/// artifact never queues behind a download. Left unbounded that opened every
/// destination in the batch at the same instant: installing a Minecraft
/// version's assets is thousands of objects, which exhausts the 1024 descriptor
/// limit a desktop session hands out and surfaces as an unreadable cache entry.
#[tokio::test]
async fn a_batch_of_cached_artifacts_bounds_the_files_it_opens() {
    /// Only descriptors pointing into `directory`, so the rest of the test
    /// binary's sockets and files cannot move the count.
    fn open_under(directory: &Path) -> usize {
        let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
            return 0;
        };
        entries
            .flatten()
            .filter(|entry| {
                std::fs::read_link(entry.path()).is_ok_and(|target| target.starts_with(directory))
            })
            .count()
    }

    // Large enough that hashing them takes long enough to observe, and more of
    // them than the probe is allowed to hold open.
    const FILES: usize = 300;
    let body = vec![b'a'; 256 * 1024];
    let digest = sha1_of(&body);

    let server = MockServer::start().await;
    let dir = TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();
    let mut requests = Vec::with_capacity(FILES);
    for index in 0..FILES {
        let destination = root.join(format!("cached-{index}.jar"));
        std::fs::write(&destination, &body).expect("seed the cache entry");
        requests.push(
            request(&server, &format!("/cached-{index}.jar"), &destination)
                .verifying(digest.clone())
                .sized(body.len() as u64),
        );
    }

    let watched = root.clone();
    let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = Arc::clone(&peak);
    let sampler = tokio::spawn(async move {
        loop {
            let open = open_under(&watched);
            observed.fetch_max(open, std::sync::atomic::Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    let (sink, _events) = ChannelSink::new();
    let sink: Arc<dyn ProgressSink> = sink;
    let reporter = OperationReporter::new("download-batch", true, sink);
    let outcomes = manager_with(RetryPolicy::none(), 6)
        .download_all(&requests, &reporter, &CancellationToken::new())
        .await
        .expect("every artifact is already cached");
    sampler.abort();

    assert_eq!(outcomes.len(), FILES);
    // No request reached the server, which mounted no route to answer one.
    assert!(
        outcomes
            .iter()
            .all(|outcome| outcome.source == ArtifactSource::AlreadyCached)
    );
    let peak = peak.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        peak < FILES / 2,
        "the batch held {peak} of {FILES} cached files open at once"
    );
}

#[tokio::test]
async fn a_failure_in_a_batch_cancels_the_rest() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing.jar"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slow.jar"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(BODY)
                .set_delay(Duration::from_secs(3)),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let requests = vec![
        request(&server, "/missing.jar", &dir.path().join("missing.jar")),
        request(&server, "/slow.jar", &dir.path().join("slow.jar")),
    ];

    let started = Instant::now();
    let error = manager(RetryPolicy::none())
        .download_all(
            &requests,
            &OperationReporter::silent("download-batch"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the missing file fails the batch");

    assert_eq!(error.index(), 0);
    assert!(matches!(
        error.error(),
        DownloadError::HttpStatus { status: 404, .. }
    ));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the sibling transfer should have been canceled, not waited out"
    );
    assert!(!dir.path().join("slow.jar").exists());
}

/// Mounts a 200 for every `ok` route and a 404 for every `missing` one.
async fn serve(server: &MockServer, ok: &[&str], missing: &[&str]) {
    for route in ok {
        Mock::given(method("GET"))
            .and(path(*route))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
            .mount(server)
            .await;
    }
    for route in missing {
        Mock::given(method("GET"))
            .and(path(*route))
            .respond_with(ResponseTemplate::new(404))
            .mount(server)
            .await;
    }
}

async fn failed_batch(requests: &[DownloadRequest]) -> BatchDownloadError {
    manager(RetryPolicy::none())
        .download_all(
            requests,
            &OperationReporter::silent("download-batch"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the batch fails")
}

#[tokio::test]
async fn a_batch_failure_names_the_request_even_when_its_error_carries_no_url() {
    let server = MockServer::start().await;
    serve(&server, &["/a.jar", "/b.jar"], &[]).await;
    let dir = TempDir::new().expect("temp dir");
    let refused = reqwest::Url::parse("file:///refused.jar").expect("file url");
    let requests = vec![
        request(&server, "/a.jar", &dir.path().join("a.jar")),
        DownloadRequest::new(refused, dir.path().join("refused.jar")),
        request(&server, "/b.jar", &dir.path().join("b.jar")),
    ];

    let error = failed_batch(&requests).await;

    assert!(matches!(error.error(), DownloadError::Policy(_)));
    assert_eq!(error.index(), 1);
}

#[tokio::test]
async fn a_batch_failure_is_not_blamed_on_a_request_whose_url_prefixes_the_failed_one() {
    let server = MockServer::start().await;
    serve(&server, &["/lib.jar"], &["/lib.jar2"]).await;
    let dir = TempDir::new().expect("temp dir");
    let requests = vec![
        request(&server, "/lib.jar", &dir.path().join("lib.jar")),
        request(&server, "/lib.jar2", &dir.path().join("lib.jar2")),
    ];

    let error = failed_batch(&requests).await;

    // Both addresses appear in the message, so the text alone cannot say which request failed.
    assert!(error.to_string().contains(requests[0].url().as_str()));
    assert_eq!(error.index(), 1);
}

#[tokio::test]
async fn a_batch_failure_names_the_request_whose_query_string_was_redacted() {
    let server = MockServer::start().await;
    serve(&server, &["/a.jar"], &["/signed.jar"]).await;
    let dir = TempDir::new().expect("temp dir");
    let requests = vec![
        request(&server, "/a.jar", &dir.path().join("a.jar")),
        request(
            &server,
            "/signed.jar?token=secret",
            &dir.path().join("signed.jar"),
        ),
    ];

    let error = failed_batch(&requests).await;

    let message = error.to_string();
    assert!(!message.contains("secret"), "{message}");
    assert_eq!(error.index(), 1);
}

#[tokio::test]
async fn concurrency_is_bounded_by_the_configured_limit() {
    let server = MockServer::start().await;
    let delay = Duration::from_millis(200);
    for index in 0..6 {
        Mock::given(method("GET"))
            .and(path(format!("/file-{index}.jar")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(BODY)
                    .set_delay(delay),
            )
            .mount(&server)
            .await;
    }

    let dir = TempDir::new().expect("temp dir");
    let requests: Vec<DownloadRequest> = (0..6)
        .map(|index| {
            request(
                &server,
                &format!("/file-{index}.jar"),
                &dir.path().join(format!("file-{index}.jar")),
            )
        })
        .collect();

    let started = Instant::now();
    manager_with(RetryPolicy::none(), 2)
        .download_all(
            &requests,
            &OperationReporter::silent("download-batch"),
            &CancellationToken::new(),
        )
        .await
        .expect("the batch succeeds");
    let elapsed = started.elapsed();

    // Six 200 ms responses through two permits cannot finish in fewer than
    // three sequential rounds. Unbounded concurrency would land near 200 ms.
    assert!(
        elapsed >= delay * 2,
        "six transfers through two permits finished in {elapsed:?}, which means the semaphore was not applied"
    );
}

#[tokio::test]
async fn a_content_addressed_artifact_is_published_into_a_cache_that_does_not_exist_yet() {
    // The shape every provider download actually has: a SHA-512 digest, a
    // sharded directory nothing has created, and a `.jar` suffix.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/mod.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let cache = CacheLayout::new(dir.path().join("cache"));
    let hash = sha512_of(BODY);
    let extension = FileExtension::new("jar").expect("jar is a safe extension");
    let destination = cache.content(ContentStore::Downloads, &hash, Some(&extension));
    assert!(!destination.exists());

    let outcome = manager(RetryPolicy::none())
        .download(
            &request(&server, "/mod.jar", &destination).verifying(hash.clone()),
            &OperationReporter::silent("test"),
            &CancellationToken::new(),
        )
        .await
        .expect("the artifact is fetched, verified, and published");

    assert_eq!(outcome.source, ArtifactSource::Network);
    assert_eq!(std::fs::read(&destination).unwrap(), BODY);
    assert!(
        no_partial_siblings(&destination),
        "a published artifact leaves no scratch beside it"
    );

    // Asking again costs nothing and reuses exactly what was verified.
    let again = manager(RetryPolicy::none())
        .download(
            &request(&server, "/mod.jar", &destination).verifying(hash),
            &OperationReporter::silent("test"),
            &CancellationToken::new(),
        )
        .await
        .expect("the cached artifact is reused");
    assert_eq!(again.source, ArtifactSource::AlreadyCached);
}

#[tokio::test]
async fn a_long_destination_name_still_gets_a_temporary_file_the_filesystem_accepts() {
    // A temporary name derived by appending to the destination's own grows with
    // it, and every filesystem has a limit: an encrypted home directory refuses
    // anything over 143 bytes, which a SHA-512 cache entry plus a full-length
    // suffix exceeds. The name a transfer writes to has to be bounded instead,
    // or the download fails before its first byte.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/mod.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join(format!("{}.jar", "a".repeat(250)));

    let outcome = manager(RetryPolicy::none())
        .download(
            &request(&server, "/mod.jar", &destination),
            &OperationReporter::silent("test"),
            &CancellationToken::new(),
        )
        .await
        .expect("a long destination name does not stop the transfer");

    assert_eq!(outcome.source, ArtifactSource::Network);
    assert_eq!(std::fs::read(&destination).unwrap(), BODY);
    assert!(no_partial_siblings(&destination));
}

#[tokio::test]
async fn a_partial_file_left_by_an_earlier_run_is_never_served_as_a_cached_artifact() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/mod.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let store = dir.path().join("store");
    std::fs::create_dir_all(&store).expect("store");
    let destination = store.join("artifact.jar");
    // What a killed process leaves behind: bytes nothing verified, under a name
    // that looks related to the artifact.
    let stale = store.join("artifact.jar.part.0123456789abcdef0123456789abcdef");
    std::fs::write(&stale, b"bytes from a transfer that never finished").expect("stale");

    let outcome = manager(RetryPolicy::none())
        .download(
            &request(&server, "/mod.jar", &destination).verifying(sha1_of(BODY)),
            &OperationReporter::silent("test"),
            &CancellationToken::new(),
        )
        .await
        .expect("the artifact is fetched");

    assert_eq!(
        outcome.source,
        ArtifactSource::Network,
        "scratch beside the cache path is not a cache entry"
    );
    assert_eq!(std::fs::read(&destination).unwrap(), BODY);
    assert!(
        stale.exists(),
        "another transfer's scratch is left alone rather than deleted on its behalf"
    );
}

#[tokio::test]
async fn two_transfers_for_one_artifact_publish_it_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/mod.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("store").join("artifact.jar");
    let manager = Arc::new(manager(RetryPolicy::none()));
    let hash = sha1_of(BODY);

    let first = {
        let manager = Arc::clone(&manager);
        let request = request(&server, "/mod.jar", &destination).verifying(hash.clone());
        tokio::spawn(async move {
            manager
                .download(
                    &request,
                    &OperationReporter::silent("test"),
                    &CancellationToken::new(),
                )
                .await
        })
    };
    let second = {
        let manager = Arc::clone(&manager);
        let request = request(&server, "/mod.jar", &destination).verifying(hash);
        tokio::spawn(async move {
            manager
                .download(
                    &request,
                    &OperationReporter::silent("test"),
                    &CancellationToken::new(),
                )
                .await
        })
    };

    let first = first.await.expect("join").expect("first transfer");
    let second = second.await.expect("join").expect("second transfer");

    assert_eq!(std::fs::read(&destination).unwrap(), BODY);
    assert!(
        no_partial_siblings(&destination),
        "neither transfer leaves its scratch behind"
    );
    assert_eq!(
        [first.source, second.source]
            .iter()
            .filter(|source| **source == ArtifactSource::AlreadyCached)
            .count(),
        1,
        "the second transfer finds what the first published instead of fetching it again"
    );
}

#[tokio::test]
async fn a_filesystem_failure_names_the_step_and_the_operating_system_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/mod.jar"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(BODY))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("temp dir");
    // A directory where the finished artifact has to go, so the transfer gets
    // all the way to publication and fails there. What matters is that the
    // report says which step gave way and what the operating system said,
    // rather than "io failure at <path>" for every one of them.
    let destination = dir.path().join("store").join("artifact.jar");
    std::fs::create_dir_all(&destination).expect("blocking directory");

    let error = manager(RetryPolicy::none())
        .download(
            &request(&server, "/mod.jar", &destination).verifying(sha1_of(BODY)),
            &OperationReporter::silent("test"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the verified artifact cannot be published");

    let reported = error.to_string();
    assert!(
        reported.contains("publish the artifact"),
        "the step that failed is missing from `{reported}`"
    );
    assert!(
        reported.contains(&destination.display().to_string()),
        "the path is missing from `{reported}`"
    );
    let underlying = std::error::Error::source(&error)
        .expect("the operating system error stays available as a source")
        .to_string();
    assert!(
        reported.contains(&underlying),
        "`{reported}` does not repeat what the operating system reported, so a single log \
         line cannot be acted on"
    );
    assert!(
        no_partial_siblings(&destination),
        "a failed transfer takes its scratch with it"
    );
}
