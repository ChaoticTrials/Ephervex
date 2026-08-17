//! The download manager.
//!
//! Everything Ephervex fetches goes through here: Mojang assets and libraries,
//! loader installers, Modrinth and CurseForge pack files, managed JDK
//! archives. One implementation means one place where atomicity, hash
//! verification, retry, cancellation, and progress are correct — rather than
//! six places where five of them are.
//!
//! # The atomicity invariant
//!
//! A download is written to a unique `.part.<id>` sibling and only renamed into place
//! after every supplied hash and size has been verified. A partial or corrupt
//! artifact therefore never appears at the final path, and that is the
//! invariant the whole shared-cache design rests on: downstream consumers check for a
//! file at a cache path and treat its presence as proof that a verified
//! artifact is there. `rename(2)` within one directory is
//! atomic on every filesystem Ephervex supports, and the `.part` file lives in
//! the destination's own directory precisely so the rename never crosses a
//! filesystem boundary.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use ephervex_model::{Hash, OperationStatus, Progress, SecretString};
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use reqwest::header::{
    ACCEPT_RANGES, CONTENT_RANGE, ETAG, HeaderName, HeaderValue, IF_RANGE, RANGE,
};
use reqwest::{Client, Response, StatusCode, Url};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use crate::allowlist::{DomainAllowlist, UrlPolicyError, check_transport, redacted};
use crate::cache::PART_SUFFIX;
use crate::client::{HttpConfig, build_client};
use crate::error::{ArtifactStep, AttemptFailure, DownloadError};
use crate::progress::{ByteTally, OperationReporter};
use crate::retry::{RetryPolicy, retry_after};
use crate::verify::HashVerifier;

/// The longest filename Ephervex generates for its own scratch files.
///
/// Deliberately well below the 255 bytes a plain Linux filesystem allows. An
/// encrypted home directory — eCryptfs with filename encryption, which is what
/// an "encrypt my home directory" installer option sets up — accepts 143, and
/// the cache lives under `$XDG_CACHE_HOME`. A SHA-512 content path is already
/// 132 characters, so a temporary sibling has to be *bounded* rather than
/// derived by appending to it. The remaining margin is deliberate: it costs
/// nothing, and it leaves room for a limit stricter than the ones known today.
const MAX_TEMPORARY_NAME_BYTES: usize = 120;

type DestinationLocks = Mutex<HashMap<PathBuf, Weak<AsyncMutex<()>>>>;
static DESTINATION_LOCKS: OnceLock<DestinationLocks> = OnceLock::new();

/// Where an artifact came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactSource {
    /// The file was already at the destination. Because of the atomicity
    /// invariant, its presence is sufficient evidence that it was verified when
    /// it was written, so it is not fetched or re-hashed again.
    AlreadyCached,
    /// Fetched over the network during this call.
    Network,
}

/// The result of a completed download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadOutcome {
    pub path: PathBuf,
    pub bytes: u64,
    pub source: ArtifactSource,
}

/// A credential one narrow set of hosts is allowed to receive.
///
/// The provider-neutral answer to a single problem: one artifact source
/// authenticates its downloads, and every other source must not learn its
/// secret. Installing the header as a client default would hand it to Mojang
/// and Modrinth alongside; putting it in the URL would write it into logs and
/// caches. So it travels with the one request that needs it, and only reaches a
/// host the caller has explicitly vouched for.
///
/// Redirects are the other half of the containment and are *not* enforced here,
/// because a redirect is decided inside the HTTP client: a manager that sends
/// these must be built with a client whose redirect policy refuses to leave the
/// same host set — see
/// [`build_allowlisted_client`](crate::build_allowlisted_client).
#[derive(Debug, Clone)]
pub struct AuthenticatedSource {
    header: HeaderName,
    secret: SecretString,
    hosts: DomainAllowlist,
}

impl AuthenticatedSource {
    /// A credential header and the hosts permitted to receive it.
    ///
    /// The header name is `'static` rather than caller-supplied text: a
    /// credential header is part of a provider's protocol, not something a
    /// remote answer gets to choose.
    #[must_use]
    pub fn new(header: HeaderName, secret: SecretString, hosts: DomainAllowlist) -> Self {
        Self {
            header,
            secret,
            hosts,
        }
    }

    /// Whether this address may receive the credential.
    #[must_use]
    pub fn permits(&self, url: &Url) -> bool {
        self.hosts.validate_url(url).is_ok()
    }

    /// The header name, for tests and diagnostics. Never the value.
    #[must_use]
    pub fn header(&self) -> &HeaderName {
        &self.header
    }

    /// The header this request should carry, or `None` when the address is not
    /// one this credential may be sent to.
    ///
    /// The value is marked sensitive, which keeps it out of the HTTP/2 header
    /// index where it would otherwise be retained for the life of the
    /// connection.
    fn header_value(&self, url: &Url) -> Option<Result<HeaderValue, DownloadError>> {
        if !self.permits(url) {
            // Fail closed on the credential rather than on the request: the
            // download may well succeed unauthenticated, and refusing to send
            // the secret is never the wrong answer.
            warn!(
                url = %redacted(url),
                "not attaching a credential to an address outside its permitted hosts"
            );
            return None;
        }
        Some(
            HeaderValue::from_str(self.secret.expose())
                .map(|mut value| {
                    value.set_sensitive(true);
                    value
                })
                // A fixed sentence, never the rejected value.
                .map_err(|_| DownloadError::CredentialUnusable { url: redacted(url) }),
        )
    }
}

/// One artifact to fetch.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    url: Url,
    destination: PathBuf,
    expected_hashes: Vec<Hash>,
    expected_size: Option<u64>,
    label: Option<String>,
    /// The credential this one request may carry, when its source needs one.
    ///
    /// Shared rather than copied: a pack import builds hundreds of requests
    /// from one operation's credential, and one secret in memory is one secret
    /// to keep out of a core dump.
    authenticated: Option<Arc<AuthenticatedSource>>,
}

impl DownloadRequest {
    /// Creates a request.
    ///
    /// The destination is supplied by the caller — normally from
    /// [`crate::CacheLayout`] — and is never derived from the URL's last path
    /// segment or from a `Content-Disposition` header. A remote server does not
    /// get to choose where a file lands.
    #[must_use]
    pub fn new(url: Url, destination: impl Into<PathBuf>) -> Self {
        Self {
            url,
            destination: destination.into(),
            expected_hashes: Vec::new(),
            expected_size: None,
            label: None,
            authenticated: None,
        }
    }

    /// Parses a URL and applies the transport policy, rejecting anything that
    /// is not HTTPS or that embeds credentials.
    pub fn parse(url: &str, destination: impl Into<PathBuf>) -> Result<Self, UrlPolicyError> {
        let parsed = Url::parse(url.trim()).map_err(|_| UrlPolicyError::Malformed {
            url: url.trim().to_owned(),
        })?;
        check_transport(&parsed, false)?;
        Ok(Self::new(parsed, destination))
    }

    /// Adds a hash to verify. Every supplied hash must match.
    #[must_use]
    pub fn verifying(mut self, hash: Hash) -> Self {
        self.expected_hashes.push(hash);
        self
    }

    /// Adds several hashes to verify.
    #[must_use]
    pub fn verifying_all(mut self, hashes: impl IntoIterator<Item = Hash>) -> Self {
        self.expected_hashes.extend(hashes);
        self
    }

    /// Declares the expected size in bytes.
    #[must_use]
    pub const fn sized(mut self, bytes: u64) -> Self {
        self.expected_size = Some(bytes);
        self
    }

    /// Sets a human-readable label for progress reporting.
    ///
    /// Display only. It is deliberately not used to build any path, because it
    /// generally originates in untrusted pack metadata.
    #[must_use]
    pub fn labeled(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Lets this request carry a provider credential.
    ///
    /// Opt-in per request, so an unauthenticated Mojang or Modrinth download
    /// built beside it cannot inherit the secret. The credential is still only
    /// sent to the hosts the source itself permits.
    #[must_use]
    pub fn authenticated_by(mut self, source: Arc<AuthenticatedSource>) -> Self {
        self.authenticated = Some(source);
        self
    }

    /// The credential this request carries, if any. For tests and diagnostics;
    /// the value inside is never rendered.
    #[must_use]
    pub fn authenticated_source(&self) -> Option<&AuthenticatedSource> {
        self.authenticated.as_deref()
    }

    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    #[must_use]
    pub fn destination(&self) -> &Path {
        &self.destination
    }

    #[must_use]
    pub fn expected_size(&self) -> Option<u64> {
        self.expected_size
    }

    #[must_use]
    pub fn expected_hashes(&self) -> &[Hash] {
        &self.expected_hashes
    }
}

/// Manager-level settings.
#[derive(Debug, Clone)]
pub struct DownloaderConfig {
    /// Maximum simultaneous transfers. Bounded so an asset index with a few
    /// thousand entries does not open a few thousand sockets.
    pub concurrency: usize,
    pub retry: RetryPolicy,
    pub http: HttpConfig,
    /// Whether to resume an interrupted transfer within a call. See
    /// [`DownloadManager`] for the conditions under which resuming is safe.
    pub allow_resume: bool,
    /// Hard ceiling on a single response body, if any. Pack metadata is
    /// untrusted, so a caller importing a bug report can cap how much disk one
    /// entry may consume.
    pub max_bytes: Option<u64>,
}

impl Default for DownloaderConfig {
    fn default() -> Self {
        Self {
            concurrency: 6,
            retry: RetryPolicy::default(),
            http: HttpConfig::default(),
            allow_resume: true,
            max_bytes: None,
        }
    }
}

/// Whether the current transfer is being tracked on its own or as part of a
/// batch.
#[derive(Debug)]
struct ProgressCursor<'a> {
    reporter: &'a OperationReporter,
    tally: Option<&'a ByteTally>,
    /// Bytes this file has currently contributed to the batch tally.
    contributed: u64,
}

impl ProgressCursor<'_> {
    fn report(&mut self, bytes: u64, file_total: Option<u64>) {
        let progress = match self.tally {
            Some(tally) => {
                let progress = tally.adjust(self.contributed, bytes);
                self.contributed = bytes;
                progress
            }
            None => Progress {
                current: bytes,
                total: file_total,
            },
        };
        self.reporter.set_progress(progress);
    }
}

/// State carried across the retry attempts of a single [`DownloadManager::download`] call.
#[derive(Debug)]
struct ResumeState {
    /// Bytes already written to the `.part` file.
    offset: u64,
    /// The strong `ETag` of the response those bytes came from, when the server
    /// supplied one.
    validator: Option<String>,
    /// Whether that response advertised `Accept-Ranges: bytes` *and* delivered
    /// its body verbatim. See [`byte_offsets_are_meaningful`] for why the
    /// second half matters.
    ranges_advertised: bool,
    /// Hash state covering exactly the first `offset` bytes of the `.part`
    /// file. Kept in lockstep with the file so a resumed transfer does not have
    /// to re-read what it already wrote.
    verifier: HashVerifier,
}

impl ResumeState {
    fn new(expected: &[Hash]) -> Self {
        Self {
            offset: 0,
            validator: None,
            ranges_advertised: false,
            verifier: HashVerifier::new(expected),
        }
    }

    fn restart(&mut self) {
        self.offset = 0;
        self.validator = None;
        self.ranges_advertised = false;
        self.verifier.reset();
    }
}

/// The reusable download subsystem.
///
/// # Resume safety
///
/// Resuming a transfer means asserting that the bytes already on disk are a
/// prefix of the bytes the server is about to send. Nothing about a plain
/// `Range` request establishes that: the resource may have changed between the
/// two requests, and the result would be a file that is a silent splice of two
/// different versions. Where hashes are supplied that corruption is caught, but
/// Sources may provide no hashes at all, so correctness cannot
/// depend on the verifier.
///
/// Resume is therefore attempted only when all of the following hold:
///
/// 1. The interrupted response carried a **strong** `ETag`. Weak validators
///    (`W/"…"`) are explicitly not usable with `If-Range` (RFC 9110 section 13.1.3):
///    they permit semantically equivalent but byte-different representations,
///    which is precisely what must not be spliced. `Last-Modified` is also not
///    used — its one-second granularity makes it a strong validator only under
///    conditions the client cannot verify.
/// 2. The request carries `If-Range: <that etag>` alongside `Range`, so the
///    *server* decides: `206` only if the representation is unchanged, and a
///    full `200` otherwise, which this code treats as "start over".
/// 3. The interrupted response advertised `Accept-Ranges: bytes` and delivered
///    its body verbatim rather than transparently decoded — see
///    [`byte_offsets_are_meaningful`], because a `Range` counts octets of the
///    encoded representation while the `.part` file holds decoded ones.
/// 4. The returned `Content-Range` starts at exactly the offset that was asked
///    for. Anything else restarts the transfer.
/// 5. The `.part` file's length still equals the recorded offset.
///
/// Resume is scoped to a single `download` call. A `.part` file left behind by
/// an earlier process is *not* resumed: its validator died with that process,
/// so condition 1 cannot be established. Such a file is truncated and the
/// transfer starts from zero.
#[derive(Debug, Clone)]
pub struct DownloadManager {
    client: Client,
    config: DownloaderConfig,
    permits: Arc<Semaphore>,
}

impl DownloadManager {
    /// Builds a manager and its HTTP client.
    pub fn new(config: DownloaderConfig) -> Result<Self, DownloadError> {
        let client = build_client(&config.http)?;
        Ok(Self::with_client(client, config))
    }

    /// Builds a manager around an existing client, so a process can share one
    /// connection pool across subsystems.
    #[must_use]
    pub fn with_client(client: Client, config: DownloaderConfig) -> Self {
        let concurrency = config.concurrency.max(1);
        Self {
            client,
            config,
            permits: Arc::new(Semaphore::new(concurrency)),
        }
    }

    #[must_use]
    pub fn config(&self) -> &DownloaderConfig {
        &self.config
    }

    /// Fetches one artifact, reporting byte progress.
    pub async fn download(
        &self,
        request: &DownloadRequest,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<DownloadOutcome, DownloadError> {
        let mut cursor = ProgressCursor {
            reporter,
            tally: None,
            contributed: 0,
        };
        reporter.begin(request.label.clone());
        let outcome = self.fetch(request, &mut cursor, cancel).await;
        reporter.finish(match &outcome {
            Ok(_) => OperationStatus::Completed,
            Err(error) if error.is_cancellation() => OperationStatus::Canceled,
            Err(error) => OperationStatus::Failed(error.to_string()),
        });
        outcome
    }

    /// Fetches many artifacts with bounded concurrency, reporting one
    /// aggregated byte-progress bar for the batch.
    ///
    /// The first failure cancels the remaining transfers: a pack import that
    /// has already lost a required file gains nothing by continuing to hammer
    /// the CDN for the rest.
    pub async fn download_all(
        &self,
        requests: &[DownloadRequest],
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<Vec<DownloadOutcome>, DownloadError> {
        reporter.begin(None);
        let sizes: Vec<Option<u64>> = requests
            .iter()
            .map(DownloadRequest::expected_size)
            .collect();
        let tally = ByteTally::new(&sizes);
        // A child token so one failure stops the siblings without canceling
        // whatever else the caller's token governs.
        let batch = cancel.child_token();

        let mut pending: FuturesUnordered<_> = requests
            .iter()
            .enumerate()
            .map(|(index, request)| {
                let tally = &tally;
                let batch = &batch;
                async move {
                    let mut cursor = ProgressCursor {
                        reporter,
                        tally: Some(tally),
                        contributed: 0,
                    };
                    if let Some(label) = request.label.clone() {
                        reporter.set_step(Some(label));
                    }
                    (index, self.fetch(request, &mut cursor, batch).await)
                }
            })
            .collect();

        let mut outcomes: Vec<Option<DownloadOutcome>> =
            (0..requests.len()).map(|_| None).collect();
        let mut failure: Option<DownloadError> = None;
        while let Some((index, result)) = pending.next().await {
            match result {
                Ok(outcome) => outcomes[index] = Some(outcome),
                Err(error) => {
                    if failure.is_none() {
                        batch.cancel();
                        failure = Some(error);
                    }
                }
            }
        }

        if let Some(error) = failure {
            reporter.finish(if error.is_cancellation() {
                OperationStatus::Canceled
            } else {
                OperationStatus::Failed(error.to_string())
            });
            return Err(error);
        }
        reporter.finish(OperationStatus::Completed);
        Ok(outcomes.into_iter().flatten().collect())
    }

    /// The retry loop around a single artifact.
    async fn fetch(
        &self,
        request: &DownloadRequest,
        cursor: &mut ProgressCursor<'_>,
        cancel: &CancellationToken,
    ) -> Result<DownloadOutcome, DownloadError> {
        let url_label = redacted(&request.url);
        check_transport(&request.url, self.config.http.allow_plaintext_http)?;

        let destination_lock = destination_lock(&request.destination);
        let _destination_guard = destination_lock.lock().await;

        if let Some(cached) = already_cached(request, cursor).await? {
            trace!(url = %url_label, path = %request.destination.display(), "artifact already cached");
            return Ok(cached);
        }

        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| DownloadError::ShuttingDown)?;

        // Re-check under the permit: a sibling transfer in the same batch may
        // have deposited the identical content-addressed artifact while this
        // one was queued.
        if let Some(cached) = already_cached(request, cursor).await? {
            return Ok(cached);
        }

        let part = part_path(&request.destination, uuid::Uuid::new_v4());
        let mut state = ResumeState::new(&request.expected_hashes);
        let mut attempt: u32 = 0;

        let result = loop {
            attempt += 1;
            let outcome = Transfer {
                manager: self,
                request,
                part: &part,
                state: &mut state,
                cursor: &mut *cursor,
                cancel,
                url_label: url_label.clone(),
            }
            .run()
            .await;
            match outcome {
                Ok(bytes) => break Ok(bytes),
                Err(AttemptFailure {
                    error,
                    retry_after: hint,
                }) => {
                    let exhausted = attempt >= self.config.retry.max_attempts;
                    if !error.is_transient() || exhausted || cancel.is_cancelled() {
                        break Err(if error.is_transient() && exhausted && attempt > 1 {
                            DownloadError::RetriesExhausted {
                                url: url_label.clone(),
                                attempts: attempt,
                                source: Box::new(error),
                            }
                        } else {
                            error
                        });
                    }
                    let delay = self.config.retry.delay_after(attempt, hint);
                    warn!(
                        url = %url_label,
                        attempt,
                        max_attempts = self.config.retry.max_attempts,
                        delay_ms = delay.as_millis(),
                        error = %error,
                        "download failed, retrying"
                    );
                    if sleep_or_cancel(delay, cancel).await.is_err() {
                        break Err(DownloadError::Canceled {
                            url: url_label.clone(),
                        });
                    }
                }
            }
        };

        match result {
            Ok(bytes) => Ok(DownloadOutcome {
                path: request.destination.clone(),
                bytes,
                source: ArtifactSource::Network,
            }),
            Err(error) => {
                // The `.part` file is only useful for resuming inside this
                // call, and its validator does not survive the process. Leaving
                // it behind would litter the cache with bytes nothing can ever
                // use.
                if let Err(cleanup) = fs::remove_file(&part).await {
                    if cleanup.kind() != std::io::ErrorKind::NotFound {
                        debug!(path = %part.display(), error = %cleanup, "could not remove partial download");
                    }
                }
                Err(error)
            }
        }
    }
}

/// One HTTP attempt at one artifact.
///
/// Exists as a type rather than as a long function so that the four steps —
/// decide whether resuming is provable, send, stream, finalize — are separately
/// readable, and so none of them has to be handed six borrowed parameters.
struct Transfer<'a, 'c> {
    manager: &'a DownloadManager,
    request: &'a DownloadRequest,
    part: &'a Path,
    state: &'a mut ResumeState,
    cursor: &'a mut ProgressCursor<'c>,
    cancel: &'a CancellationToken,
    /// The URL as it may appear in logs and errors: query and credentials
    /// already stripped.
    url_label: String,
}

/// What the response said about where to write and how much is coming.
#[derive(Debug, Clone, Copy)]
struct BodyPlan {
    /// Offset in the `.part` file the body continues from.
    write_offset: u64,
    /// Complete size of the artifact, when the server disclosed it.
    total: Option<u64>,
}

impl Transfer<'_, '_> {
    async fn run(mut self) -> Result<u64, AttemptFailure> {
        if self.cancel.is_cancelled() {
            return Err(self.canceled());
        }
        if let Some(parent) = self.request.destination.parent() {
            fs::create_dir_all(parent).await.map_err(|source| {
                DownloadError::io(ArtifactStep::CreateDirectory, parent, source)
            })?;
        }

        let resuming = self.prepare_resume().await;
        let response = self.send(resuming).await?;
        let plan = self.plan_body(&response, resuming)?;
        let written = self.stream(response, plan).await?;
        self.finalize(written).await
    }

    /// Decides whether the resume conditions documented on [`DownloadManager`]
    /// all hold, resetting the state when they do not.
    async fn prepare_resume(&mut self) -> bool {
        let on_disk = fs::metadata(self.part).await.map(|meta| meta.len()).ok();
        let resumable = self.manager.config.allow_resume
            && self.state.offset > 0
            && self.state.validator.is_some()
            && self.state.ranges_advertised
            && on_disk == Some(self.state.offset);
        if !resumable && self.state.offset > 0 {
            debug!(url = %self.url_label, "cannot prove resume safety, restarting from byte zero");
            self.state.restart();
        }
        resumable
    }

    async fn send(&self, resuming: bool) -> Result<Response, AttemptFailure> {
        let mut builder = self.manager.client.get(self.request.url.clone());
        if let Some(source) = &self.request.authenticated
            && let Some(value) = source.header_value(&self.request.url)
        {
            builder = builder.header(source.header.clone(), value?);
        }
        if let Some(validator) = self.state.validator.clone().filter(|_| resuming) {
            // `If-Range` hands the decision to the server: it answers 206 only
            // if the representation is byte-for-byte the one those `.part`
            // bytes came from, and a plain 200 otherwise.
            builder = builder
                .header(RANGE, format!("bytes={}-", self.state.offset))
                .header(IF_RANGE, validator);
        }

        // Cancellation has to cover the request step too, not just the body:
        // a slow server that has not sent its headers yet is exactly when a
        // user reaches for the cancel button.
        let sent = tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Err(self.canceled()),
            sent = builder.send() => sent,
        };
        let response = sent.map_err(|source| DownloadError::Transport {
            url: self.url_label.clone(),
            source,
        })?;

        let status = response.status();
        if !status.is_success() {
            return Err(AttemptFailure {
                retry_after: retry_after(response.headers()),
                error: DownloadError::HttpStatus {
                    url: self.url_label.clone(),
                    status: status.as_u16(),
                },
            });
        }
        Ok(response)
    }

    /// Reconciles what was asked for with what arrived.
    fn plan_body(
        &mut self,
        response: &Response,
        resuming: bool,
    ) -> Result<BodyPlan, AttemptFailure> {
        let status = response.status();
        let partial = status == StatusCode::PARTIAL_CONTENT;

        if resuming && !partial {
            // The representation changed, so everything already written is
            // stale. This is exactly the case `If-Range` exists to signal.
            debug!(url = %self.url_label, %status, "full body returned for a ranged request, restarting");
            self.state.restart();
        }
        if !resuming && partial {
            // A 206 nobody asked for: refusing beats guessing which part of the
            // resource this is.
            return Err(self.range_not_honored());
        }

        let mut plan = BodyPlan {
            write_offset: 0,
            total: response.content_length(),
        };
        if partial {
            match parse_content_range(response) {
                Some((start, complete)) if start == self.state.offset => {
                    plan.write_offset = start;
                    plan.total = complete;
                }
                _ => {
                    self.state.restart();
                    return Err(self.range_not_honored());
                }
            }
        }

        self.state.validator = strong_etag(response);
        self.state.ranges_advertised = byte_offsets_are_meaningful(response);
        debug!(
            url = %self.url_label,
            %status,
            resumed_from = plan.write_offset,
            total = ?plan.total,
            "streaming download"
        );
        Ok(plan)
    }

    /// Writes the body into the `.part` file, hashing as it goes.
    async fn stream(&mut self, response: Response, plan: BodyPlan) -> Result<u64, AttemptFailure> {
        let mut file = self.open_part(plan.write_offset).await?;
        let mut written = plan.write_offset;
        self.state.offset = plan.write_offset;

        let mut stream = Box::pin(response.bytes_stream());
        loop {
            let next = tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Err(self.canceled()),
                next = stream.next() => next,
            };
            let Some(chunk) = next else { break };
            let chunk = chunk.map_err(|source| DownloadError::Transport {
                url: self.url_label.clone(),
                source,
            })?;

            written = written.saturating_add(chunk.len() as u64);
            if let Some(limit) = self.manager.config.max_bytes {
                if written > limit {
                    return Err(DownloadError::BodyTooLarge {
                        url: self.url_label.clone(),
                        limit,
                    }
                    .into());
                }
            }
            // Stop as soon as the body overruns its declared size rather than
            // reading an unbounded response to the end just to reject it.
            if let Some(expected) = self.request.expected_size {
                if written > expected {
                    return Err(self.size_mismatch(expected, written));
                }
            }

            file.write_all(&chunk)
                .await
                .map_err(|source| DownloadError::io(ArtifactStep::WriteBody, self.part, source))?;
            // Hashed only after the write succeeds, so the verifier state always
            // describes exactly the first `state.offset` bytes of the file —
            // which is what makes resuming without re-reading them sound.
            self.state.verifier.update(&chunk);
            self.state.offset = written;
            self.cursor.report(written, plan.total);
        }

        file.flush()
            .await
            .map_err(|source| DownloadError::io(ArtifactStep::Flush, self.part, source))?;
        // Durable before the rename: a crash must not be able to leave a
        // renamed-but-empty artifact at a cache path.
        file.sync_all()
            .await
            .map_err(|source| DownloadError::io(ArtifactStep::Flush, self.part, source))?;
        Ok(written)
    }

    /// Verifies, then renames into place.
    async fn finalize(&mut self, written: u64) -> Result<u64, AttemptFailure> {
        if let Some(expected) = self.request.expected_size {
            if written != expected {
                return Err(self.size_mismatch(expected, written));
            }
        }

        // Taken out so the digests are finalized exactly once; any retry from
        // here restarts at byte zero anyway.
        let verifier = std::mem::replace(
            &mut self.state.verifier,
            HashVerifier::new(&self.request.expected_hashes),
        );
        if let Err(mismatch) = verifier.finish() {
            // Bad bytes must not survive where a later attempt could mistake
            // them for a valid prefix.
            self.discard_part("mismatched download").await;
            self.state.restart();
            warn!(
                url = %self.url_label,
                algorithm = %mismatch.algorithm,
                "hash mismatch, discarding artifact"
            );
            return Err(DownloadError::HashMismatch {
                url: self.url_label.clone(),
                algorithm: mismatch.algorithm,
                expected: mismatch.expected,
                actual: mismatch.actual,
            }
            .into());
        }

        fs::rename(self.part, &self.request.destination)
            .await
            .map_err(|source| {
                DownloadError::io(ArtifactStep::Publish, &self.request.destination, source)
            })?;
        sync_parent_directory(&self.request.destination).await;
        self.cursor.report(written, Some(written));
        Ok(written)
    }

    /// Opens the `.part` file positioned at `offset`, truncating when starting
    /// over.
    async fn open_part(&self, offset: u64) -> Result<File, DownloadError> {
        let mut options = OpenOptions::new();
        options.write(true).create(true);
        if offset == 0 {
            options.truncate(true);
        }
        let mut file = options.open(self.part).await.map_err(|source| {
            DownloadError::io(ArtifactStep::CreateTemporary, self.part, source)
        })?;
        if offset > 0 {
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|source| {
                    DownloadError::io(ArtifactStep::CreateTemporary, self.part, source)
                })?;
        }
        Ok(file)
    }

    async fn discard_part(&self, reason: &str) {
        if let Err(error) = fs::remove_file(self.part).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                debug!(path = %self.part.display(), %error, "could not remove {reason}");
            }
        }
    }

    fn canceled(&self) -> AttemptFailure {
        DownloadError::Canceled {
            url: self.url_label.clone(),
        }
        .into()
    }

    fn range_not_honored(&self) -> AttemptFailure {
        DownloadError::RangeNotHonored {
            url: self.url_label.clone(),
        }
        .into()
    }

    fn size_mismatch(&self, expected: u64, actual: u64) -> AttemptFailure {
        DownloadError::SizeMismatch {
            url: self.url_label.clone(),
            expected,
            actual,
        }
        .into()
    }
}

/// Reports an artifact that is already at its destination.
///
/// Expectations are checked again because logical cache paths and files copied
/// in by older builds do not carry proof of which request verified them.
async fn already_cached(
    request: &DownloadRequest,
    cursor: &mut ProgressCursor<'_>,
) -> Result<Option<DownloadOutcome>, DownloadError> {
    let destination = &request.destination;
    let metadata = match fs::symlink_metadata(destination).await {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(DownloadError::io(
                ArtifactStep::ReadCached,
                destination,
                source,
            ));
        }
    };
    let bytes = metadata.len();
    let size_matches = request
        .expected_size
        .is_none_or(|expected| expected == bytes);
    let hashes_match = if request.expected_hashes.is_empty() {
        true
    } else {
        let mut file = File::open(destination)
            .await
            .map_err(|source| DownloadError::io(ArtifactStep::ReadCached, destination, source))?;
        let mut verifier = HashVerifier::new(&request.expected_hashes);
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).await.map_err(|source| {
                DownloadError::io(ArtifactStep::ReadCached, destination, source)
            })?;
            if read == 0 {
                break;
            }
            verifier.update(&buffer[..read]);
        }
        verifier.finish().is_ok()
    };

    if !size_matches || !hashes_match {
        warn!(path = %destination.display(), "discarding a cache entry that does not match its request");
        fs::remove_file(destination)
            .await
            .map_err(|source| DownloadError::io(ArtifactStep::ReadCached, destination, source))?;
        return Ok(None);
    }

    cursor.report(bytes, Some(bytes));
    Ok(Some(DownloadOutcome {
        path: destination.clone(),
        bytes,
        source: ArtifactSource::AlreadyCached,
    }))
}

/// A unique partial sibling in the destination's own directory.
///
/// A sibling because the publishing rename must not cross a filesystem
/// boundary, and unique because two transfers may aim at one content-addressed
/// destination at once and must not write over each other's bytes.
///
/// The destination's own name is kept only as far as
/// [`MAX_TEMPORARY_NAME_BYTES`] allows. It is there so a human looking at the
/// cache can tell what a transfer in flight belongs to; what makes the name
/// unique is the identifier, so shortening the readable half costs nothing and
/// is what keeps the result creatable on every filesystem the cache can land
/// on.
fn part_path(destination: &Path, id: uuid::Uuid) -> PathBuf {
    // `simple` rather than the hyphenated form: the same 128 bits, four bytes
    // shorter, and every byte here is taken from the readable half.
    let suffix = format!(".{PART_SUFFIX}.{}", id.simple());
    let name = destination
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let kept = bounded(&name, MAX_TEMPORARY_NAME_BYTES.saturating_sub(suffix.len()));

    destination
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(format!("{kept}{suffix}"))
}

/// The longest prefix of `name` that fits `max_bytes` without splitting a
/// character.
fn bounded(name: &str, max_bytes: usize) -> &str {
    if name.len() <= max_bytes {
        return name;
    }
    let mut end = max_bytes;
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

fn destination_lock(destination: &Path) -> Arc<AsyncMutex<()>> {
    let locks = DESTINATION_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(destination).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(AsyncMutex::new(()));
    locks.insert(destination.to_path_buf(), Arc::downgrade(&lock));
    lock
}

/// Waits, unless the caller cancels first.
async fn sleep_or_cancel(delay: Duration, cancel: &CancellationToken) -> Result<(), ()> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(()),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

/// Returns the response's `ETag` only if it is a strong validator.
///
/// Weak validators are rejected outright rather than stored, so the resume path
/// cannot accidentally use one (RFC 9110 section 13.1.3).
fn strong_etag(response: &Response) -> Option<String> {
    let value = response.headers().get(ETAG)?.to_str().ok()?.trim();
    if value.starts_with("W/") || !value.starts_with('"') || !value.ends_with('"') {
        return None;
    }
    Some(value.to_owned())
}

/// Whether a byte offset into the `.part` file means the same thing to the
/// server as it does to us.
///
/// Two conditions, and the second one is easy to miss. `Range` counts octets of
/// the *encoded* representation, but the client stores the *decoded* body: if
/// the response arrived `Content-Encoding: gzip` and reqwest inflated it
/// transparently, then "resume from byte 1500" asks the server for a point in
/// the compressed stream that has nothing to do with byte 1500 of what is on
/// disk, and the result would be a silently spliced, corrupt artifact.
///
/// reqwest signals exactly this case: `Response::content_length` is documented
/// to return `None` when the body was automatically decoded, because the
/// declared length no longer describes what the caller receives. Requiring a
/// known length therefore rules out every transparently decoded response
/// without having to inspect headers reqwest has already consumed.
fn byte_offsets_are_meaningful(response: &Response) -> bool {
    let advertises_ranges = response
        .headers()
        .get(ACCEPT_RANGES)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("bytes"));
    advertises_ranges && response.content_length().is_some()
}

/// Parses `Content-Range: bytes <start>-<end>/<complete-or-*>`.
///
/// Returns the start offset and the complete length when the server disclosed
/// it.
fn parse_content_range(response: &Response) -> Option<(u64, Option<u64>)> {
    let value = response.headers().get(CONTENT_RANGE)?.to_str().ok()?;
    let rest = value.trim().strip_prefix("bytes ")?;
    let (range, complete) = rest.split_once('/')?;
    let (start, _end) = range.split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let complete = complete.trim().parse::<u64>().ok();
    Some((start, complete))
}

/// Best-effort durability for the rename itself.
///
/// Renaming makes the artifact visible immediately, but on a crash the
/// directory entry could be lost unless the directory is synced. Failure here
/// is logged and ignored: the artifact is already correct and visible, and some
/// filesystems refuse `fsync` on a directory handle.
async fn sync_parent_directory(destination: &Path) {
    let Some(parent) = destination.parent() else {
        return;
    };
    match File::open(parent).await {
        Ok(dir) => {
            if let Err(error) = dir.sync_all().await {
                trace!(path = %parent.display(), %error, "directory fsync unavailable");
            }
        }
        Err(error) => trace!(path = %parent.display(), %error, "could not open directory to fsync"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// The shortest filename limit Ephervex has to work under.
    ///
    /// eCryptfs with filename encryption, which is what an "encrypt my home
    /// directory" installer option sets up, accepts 143 bytes where a plain
    /// Linux filesystem accepts 255. The cache lives under `$XDG_CACHE_HOME`,
    /// so that is the limit that binds.
    const ENCRYPTED_HOME_NAME_LIMIT: usize = 143;

    /// The generated bound must stay inside the shortest limit Ephervex
    /// supports, so raising it past what an encrypted home accepts fails the
    /// build rather than the next download.
    const _: () = assert!(MAX_TEMPORARY_NAME_BYTES <= ENCRYPTED_HOME_NAME_LIMIT);

    #[test]
    fn a_temporary_artifact_name_fits_an_encrypted_home_directory() {
        // A SHA-512 content path is already 132 characters before any suffix,
        // and every mod a provider publishes a SHA-512 for lands on one. Naming
        // the temporary sibling after it and appending a full-length unique
        // suffix produced 174 characters, which an encrypted home refuses to
        // create at all: every such download failed before its first byte.
        let destination = PathBuf::from(format!(
            "/cache/ephervex/downloads/sha512/aa/{}.jar",
            "a".repeat(128)
        ));
        let part = part_path(&destination, uuid::Uuid::new_v4());
        let name = part
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .expect("a temporary artifact has a name");

        assert!(
            name.len() <= MAX_TEMPORARY_NAME_BYTES,
            "`{name}` is {} bytes, over the {MAX_TEMPORARY_NAME_BYTES} this launcher generates",
            name.len()
        );
    }

    #[test]
    fn the_part_file_is_a_sibling_of_the_destination() {
        let id = uuid::Uuid::nil();
        let part = part_path(Path::new("/cache/ephervex/downloads/sha1/da/abc.jar"), id);
        assert_eq!(
            part.file_name().and_then(std::ffi::OsStr::to_str),
            Some("abc.jar.part.00000000000000000000000000000000"),
            "a name that already fits keeps the destination's own name in full"
        );
        assert_eq!(
            part.parent(),
            Path::new("/cache/ephervex/downloads/sha1/da/abc.jar").parent(),
            "rename must not cross a filesystem boundary"
        );
    }

    #[test]
    fn a_request_refuses_a_non_https_url_at_construction() {
        let error = DownloadRequest::parse("http://example.invalid/a.jar", "/tmp/a.jar")
            .expect_err("plaintext must be refused");
        assert!(matches!(error, UrlPolicyError::SchemeNotPermitted { .. }));
    }

    #[test]
    fn a_request_refuses_a_file_url_at_construction() {
        let error = DownloadRequest::parse("file:///etc/passwd", "/tmp/a")
            .expect_err("file urls must be refused");
        assert!(matches!(error, UrlPolicyError::SchemeNotPermitted { .. }));
    }

    #[test]
    fn a_label_never_influences_the_destination() {
        let request = DownloadRequest::parse("https://example.invalid/a.jar", "/cache/fixed.jar")
            .expect("https url")
            .labeled("../../../../etc/cron.d/evil");
        assert_eq!(request.destination(), Path::new("/cache/fixed.jar"));
    }

    // -- credentials on artifact downloads ---------------------------------

    /// Obviously not a credential.
    const FAKE_KEY: &str = "test-provider-key-do-not-use";

    fn curseforge_source() -> Arc<AuthenticatedSource> {
        Arc::new(AuthenticatedSource::new(
            HeaderName::from_static("x-api-key"),
            SecretString::new(FAKE_KEY),
            DomainAllowlist::curseforge_artifact_defaults(),
        ))
    }

    fn url(address: &str) -> Url {
        Url::parse(address).expect("a url")
    }

    #[test]
    fn a_credential_is_offered_only_to_the_hosts_its_source_vouches_for() {
        let source = curseforge_source();
        assert!(source.permits(&url("https://mediafilez.forgecdn.net/files/1/2/a.jar")));
        assert!(source.permits(&url("https://edge.forgecdn.net/files/1/2/a.jar")));

        for elsewhere in [
            // The API origin is not a delivery host.
            "https://api.curseforge.com/v1/mods/1/files/2",
            // A look-alike host, and a subdomain of a permitted one.
            "https://evil.example/files/1/2/a.jar",
            "https://forgecdn.net.evil.example/a.jar",
            "https://cdn.modrinth.com/data/x/a.jar",
            // Plaintext, which the transport rules refuse whoever the host is.
            "http://mediafilez.forgecdn.net/files/1/2/a.jar",
        ] {
            assert!(
                !source.permits(&url(elsewhere)),
                "{elsewhere} was offered a credential"
            );
        }
    }

    #[test]
    fn an_address_outside_the_permitted_hosts_gets_no_header_rather_than_an_error() {
        // Failing closed on the credential and open on the request: the
        // download may well succeed without it, and not sending a secret is
        // never the wrong answer.
        let source = curseforge_source();
        assert!(
            source
                .header_value(&url("https://evil.example/a.jar"))
                .is_none()
        );
        let permitted = source
            .header_value(&url("https://mediafilez.forgecdn.net/files/1/2/a.jar"))
            .expect("a permitted host is offered the header")
            .expect("the key builds a header");
        assert!(
            permitted.is_sensitive(),
            "a credential header must stay out of the HTTP/2 header index"
        );
    }

    #[test]
    fn an_ordinary_download_carries_no_credential_at_all() {
        // Structural: the credential is opt-in per request, so a Mojang or
        // Modrinth request built beside an authenticated one cannot inherit it.
        let plain = DownloadRequest::parse("https://cdn.modrinth.com/data/x/a.jar", "/cache/a.jar")
            .expect("https url");
        assert!(plain.authenticated_source().is_none());

        let authenticated = DownloadRequest::parse(
            "https://mediafilez.forgecdn.net/files/1/2/a.jar",
            "/cache/b.jar",
        )
        .expect("https url")
        .authenticated_by(curseforge_source());
        assert_eq!(
            authenticated
                .authenticated_source()
                .map(|source| source.header().as_str()),
            Some("x-api-key")
        );
    }

    #[test]
    fn nothing_about_an_authenticated_request_renders_the_credential() {
        let request = DownloadRequest::parse(
            "https://mediafilez.forgecdn.net/files/1/2/a.jar",
            "/cache/b.jar",
        )
        .expect("https url")
        .authenticated_by(curseforge_source());

        assert!(!format!("{request:?}").contains(FAKE_KEY), "{request:?}");
        assert!(!request.url().as_str().contains(FAKE_KEY));
        assert!(
            !DownloadError::CredentialUnusable {
                url: redacted(request.url()),
            }
            .to_string()
            .contains(FAKE_KEY)
        );
    }

    #[test]
    fn concurrency_is_never_configured_below_one() {
        let config = DownloaderConfig {
            concurrency: 0,
            ..DownloaderConfig::default()
        };
        let manager = DownloadManager::new(config).expect("client builds");
        assert_eq!(manager.permits.available_permits(), 1);
    }
}
