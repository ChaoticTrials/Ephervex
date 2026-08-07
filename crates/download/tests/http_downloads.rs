//! Integration tests for the download manager against a local HTTP server
//! (the download subsystem regression coverage).
//!
//! Everything here runs against `wiremock` on loopback. No test contacts
//! Mojang, Modrinth, CurseForge, Forge, NeoForge, or Microsoft, so ordinary CI
//! never depends on a live service and `cargo test --workspace --offline`
//! passes once dependencies are vendored.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use ephervex_download::{
    ArtifactSource, ChannelSink, DownloadError, DownloadManager, DownloadRequest, DownloaderConfig,
    HttpConfig, OperationReporter, ProgressSink, RetryPolicy, UrlPolicyError,
};
use ephervex_model::{Hash, HashAlgorithm, OperationStatus};
use sha1::{Digest, Sha1};
use tempfile::TempDir;
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

fn part_of(destination: &std::path::Path) -> std::path::PathBuf {
    let mut name = destination.as_os_str().to_os_string();
    name.push(".part");
    std::path::PathBuf::from(name)
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
        !part_of(&destination).exists(),
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
    assert!(!part_of(&destination).exists());
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
    assert!(!part_of(&destination).exists());
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
    assert!(!part_of(&destination).exists());
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
    assert!(agent.contains("github.com/ChaoticTrials/ephervex"));
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

    assert!(matches!(
        error,
        DownloadError::HttpStatus { status: 404, .. }
    ));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the sibling transfer should have been canceled, not waited out"
    );
    assert!(!dir.path().join("slow.jar").exists());
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
