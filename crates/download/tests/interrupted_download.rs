//! Interrupted-download behavior ("interrupted download").
//!
//! A mock-HTTP library can only produce well-formed responses, so it cannot
//! express the case that matters most here: a server that announces a
//! `Content-Length`, sends part of the body, and then drops the connection.
//! These tests therefore drive a hand-rolled loopback TCP server, which is
//! still entirely offline — no test here contacts a real host.
//!
//! The two behaviors under test enforce "resume only when
//! server semantics make it safe":
//!
//! - with a **strong `ETag`** and `Accept-Ranges: bytes`, the retry resumes via
//!   `Range` + `If-Range` and the artifact is completed correctly;
//! - with a **weak validator**, resuming is not provable, so the retry starts
//!   over from byte zero rather than splicing two representations together.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ephervex_download::{
    DownloadManager, DownloadRequest, DownloaderConfig, HttpConfig, OperationReporter, RetryPolicy,
};
use ephervex_model::{Hash, HashAlgorithm};
use sha1::{Digest, Sha1};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// Long enough that the cut is unambiguous, short enough to stay readable.
fn body() -> Vec<u8> {
    (0..4096_u32)
        .map(|index| u8::try_from(index % 251).unwrap_or_default())
        .collect()
}

fn sha1_of(bytes: &[u8]) -> Hash {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    Hash::new(HashAlgorithm::Sha1, &hex::encode(hasher.finalize())).expect("computed sha1 is valid")
}

fn manager() -> DownloadManager {
    DownloadManager::new(DownloaderConfig {
        concurrency: 2,
        retry: RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(20),
            multiplier: 2,
        },
        http: HttpConfig::for_loopback_tests(),
        allow_resume: true,
        max_bytes: None,
    })
    .expect("client builds")
}

/// Reads one request head and returns it as text.
async fn read_request_head(stream: &mut TcpStream) -> String {
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

/// Pulls the start offset out of `Range: bytes=<start>-`.
fn requested_range_start(head: &str) -> Option<u64> {
    head.lines()
        .find(|line| line.to_ascii_lowercase().starts_with("range:"))
        .and_then(|line| line.split_once('=').map(|(_, rest)| rest.to_owned()))
        .and_then(|rest| rest.trim().trim_end_matches('-').parse().ok())
}

fn has_if_range(head: &str) -> bool {
    head.lines()
        .any(|line| line.to_ascii_lowercase().starts_with("if-range:"))
}

/// A server that truncates its first response and, when asked, honors a range
/// request for the remainder.
///
/// `validator` is written out verbatim as the `ETag`, so a test can supply a
/// weak one and observe that resuming is refused.
struct TruncatingServer {
    address: std::net::SocketAddr,
    requests: Arc<AtomicUsize>,
    ranged_requests: Arc<AtomicUsize>,
}

impl TruncatingServer {
    async fn spawn(body: Vec<u8>, cut_at: usize, validator: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("local address");
        let requests = Arc::new(AtomicUsize::new(0));
        let ranged_requests = Arc::new(AtomicUsize::new(0));

        let counters = (Arc::clone(&requests), Arc::clone(&ranged_requests));
        tokio::spawn(async move {
            let (requests, ranged_requests) = counters;
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let head = read_request_head(&mut stream).await;
                requests.fetch_add(1, Ordering::SeqCst);
                let total = body.len();

                let range_start = requested_range_start(&head)
                    .and_then(|start| usize::try_from(start).ok())
                    .filter(|start| *start < total);
                match range_start {
                    Some(start) if has_if_range(&head) => {
                        ranged_requests.fetch_add(1, Ordering::SeqCst);
                        let header = format!(
                            "HTTP/1.1 206 Partial Content\r\n\
                             Content-Length: {}\r\n\
                             Content-Range: bytes {start}-{}/{total}\r\n\
                             Accept-Ranges: bytes\r\n\
                             ETag: {validator}\r\n\
                             Connection: close\r\n\r\n",
                            total - start,
                            total - 1,
                        );
                        let _ = stream.write_all(header.as_bytes()).await;
                        let _ = stream.write_all(&body[start..]).await;
                    }
                    _ => {
                        // Announce the full length, then deliver only part of
                        // it and hang up: a dropped transfer, not a short file.
                        let header = format!(
                            "HTTP/1.1 200 OK\r\n\
                             Content-Length: {total}\r\n\
                             Accept-Ranges: bytes\r\n\
                             ETag: {validator}\r\n\
                             Connection: close\r\n\r\n"
                        );
                        let _ = stream.write_all(header.as_bytes()).await;
                        let _ = stream.write_all(&body[..cut_at]).await;
                    }
                }
                let _ = stream.flush().await;
                let _ = stream.shutdown().await;
            }
        });

        Self {
            address,
            requests,
            ranged_requests,
        }
    }

    fn url(&self) -> String {
        format!("http://{}/artifact.jar", self.address)
    }
}

#[tokio::test]
async fn an_interrupted_download_resumes_from_the_byte_it_stopped_at() {
    let payload = body();
    let server = TruncatingServer::spawn(payload.clone(), 1500, "\"v1\"").await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    let url = reqwest::Url::parse(&server.url()).expect("loopback url");
    let outcome = manager()
        .download(
            &DownloadRequest::new(url, &destination)
                .verifying(sha1_of(&payload))
                .sized(payload.len() as u64),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect("the retry resumes and completes the artifact");

    assert_eq!(outcome.bytes, payload.len() as u64);
    assert_eq!(
        std::fs::read(&destination).expect("artifact exists"),
        payload,
        "a resumed transfer must reassemble the exact original bytes"
    );
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);
    assert_eq!(
        server.ranged_requests.load(Ordering::SeqCst),
        1,
        "the second attempt should have been a range request, not a full refetch"
    );
}

#[tokio::test]
async fn a_weak_validator_forces_a_restart_instead_of_a_resume() {
    let payload = body();
    // `W/"v1"` is a weak validator: RFC 9110 section 13.1.3 forbids using it with
    // `If-Range`, because it permits byte-different representations and
    // splicing two of them would silently corrupt the artifact.
    let server = TruncatingServer::spawn(payload.clone(), 1500, "W/\"v1\"").await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("artifact.jar");
    let url = reqwest::Url::parse(&server.url()).expect("loopback url");
    let error = manager()
        .download(
            &DownloadRequest::new(url, &destination)
                .verifying(sha1_of(&payload))
                .sized(payload.len() as u64),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("this server never delivers a whole body without a range request");

    assert!(
        error.is_transient()
            || matches!(
                error,
                ephervex_download::DownloadError::RetriesExhausted { .. }
            )
    );
    assert_eq!(
        server.ranged_requests.load(Ordering::SeqCst),
        0,
        "resuming must not be attempted when safety cannot be established"
    );
    assert!(
        !destination.exists(),
        "an interrupted download must never appear at the final path"
    );
}

#[tokio::test]
async fn an_interrupted_download_leaves_nothing_at_the_final_path() {
    let payload = body();
    let server = TruncatingServer::spawn(payload.clone(), 1500, "W/\"v1\"").await;

    let dir = TempDir::new().expect("temp dir");
    let destination = dir.path().join("nested").join("artifact.jar");
    let url = reqwest::Url::parse(&server.url()).expect("loopback url");
    let _ = manager()
        .download(
            &DownloadRequest::new(url, &destination).verifying(sha1_of(&payload)),
            &OperationReporter::silent("download"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the body is never delivered in full");

    assert!(!destination.exists());
    let mut part = destination.as_os_str().to_os_string();
    part.push(".part");
    assert!(
        !std::path::Path::new(&part).exists(),
        "a partial file whose validator did not survive is useless and must be cleaned up"
    );
}
