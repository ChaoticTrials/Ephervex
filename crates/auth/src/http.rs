//! The HTTP plumbing shared by all five legs of the chain.
//!
//! # There is still only one HTTP client in the workspace
//!
//! `ephervex-download`'s [`build_origin_contained_client`] is what constructs
//! it, so every request made here inherits the same rustls stack, connect and
//! read timeouts, and launcher `User-Agent` as every artifact download. Its
//! redirect policy is stricter than the ordinary client's: bounded, and never
//! leaving the origin a request was sent to, because a `307` or `308` would
//! replay a credential-carrying body there. A bare `reqwest::Client::new()` in
//! this crate would opt out of all of it at once, and the redirect policy is the
//! part that would go unnoticed longest.
//!
//! What this crate does *not* reuse is [`DownloadManager`], and the reason is
//! shape rather than principle: that type exists to stream a body to a path,
//! verify a hash, and finalize atomically. Authentication posts form and JSON
//! bodies and reads small JSON documents into memory; there is no destination
//! path, no publisher-supplied digest, and nothing to finalize. Forcing the
//! chain through it would mean writing OAuth tokens to disk in order to read
//! them back, which would expose secrets in user-visible diagnostics.
//!
//! [`build_origin_contained_client`]: ephervex_download::build_origin_contained_client
//! [`DownloadManager`]: ephervex_download::DownloadManager
//!
//! # Redaction is structural
//!
//! [`post_json`] and friends take already-built request builders and never log
//! a body, a header, or a response payload - only the stage, the status, and
//! the URL with its query string removed via `ephervex_download::redacted`.
//! Bodies here contain tokens by definition, so "do not log the body" is a rule
//! this module enforces rather than one each call site has to remember

use std::time::Duration;

use ephervex_download::{HttpConfig, redacted, status_is_transient};
use reqwest::{Client, RequestBuilder, Response, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::error::{AuthError, AuthResult};
use crate::stage::AuthStage;

/// How long any single authentication request may take.
///
/// Shorter than a download's: nothing in this chain returns more than a few
/// kilobytes, and a user staring at a login button deserves a failure rather
/// than a spinner.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The most bytes of response body any request in the chain may deliver.
///
/// Token, Xbox, and profile documents are a few kilobytes; the profile with
/// skin and cape lists is the largest. The ceiling is applied to the decoded
/// body, so a compressed response cannot expand past it.
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// Builds the HTTP client the chain uses.
///
/// Every request carries a credential in its body, so a redirect that leaves
/// the origin the request was sent to is refused rather than replayed.
pub fn build_client(config: &HttpConfig) -> AuthResult<Client> {
    ephervex_download::build_origin_contained_client(config).map_err(|source| {
        AuthError::ClientBuild {
            source: Box::new(source),
        }
    })
}

/// A response the caller still has to interpret: the status plus the raw body.
///
/// Deliberately *not* pre-classified into success and failure. Three legs of
/// this chain use documented non-2xx statuses as ordinary control flow - the
/// device-code poll answers `400 authorization_pending` until the user is done,
/// XSTS answers `401` with an `XErr` explaining the account, and the profile
/// endpoint answers `404` to say the account does not own the game. A helper
/// that turned every 4xx into an error would erase all three.
#[derive(Debug)]
pub struct RawResponse {
    pub status: u16,
    /// Never logged. Every body in this chain either is a token or contains
    /// one.
    pub body: String,
}

impl RawResponse {
    /// Whether the status is 2xx.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// Parses the body as JSON, attributing a syntax error to the right leg.
    pub fn parse<T: DeserializeOwned>(&self, stage: AuthStage) -> AuthResult<T> {
        parse(stage, &self.body)
    }
}

/// Sends a request and reads the whole body, honoring cancellation until the
/// last byte has arrived and refusing a body over [`MAX_RESPONSE_BYTES`].
pub async fn send(
    stage: AuthStage,
    request: RequestBuilder,
    url: &Url,
    cancel: &CancellationToken,
) -> AuthResult<RawResponse> {
    let request = request.timeout(REQUEST_TIMEOUT);
    let mut response: Response = tokio::select! {
        // Biased so that a token canceled before the request even starts wins
        // rather than racing a network round trip it is going to discard.
        biased;
        () = cancel.cancelled() => return Err(AuthError::Canceled),
        result = request.send() => result.map_err(|source| {
            debug!(stage = stage.as_str(), url = %redacted(url), "authentication request failed");
            AuthError::transport(stage, source)
        })?,
    };

    let status = response.status().as_u16();
    let too_large = || AuthError::ResponseTooLarge {
        stage,
        limit: MAX_RESPONSE_BYTES,
    };
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(too_large());
    }

    // Content-Length may be absent or wrong, so the ceiling is enforced on the
    // bytes actually received.
    let mut bytes: Vec<u8> = Vec::new();
    loop {
        let chunk = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(AuthError::Canceled),
            result = response.chunk() => {
                result.map_err(|source| AuthError::transport(stage, source))?
            }
        };
        let Some(chunk) = chunk else { break };
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    // A cancellation that lands as the last chunk does must not turn into a
    // success.
    if cancel.is_cancelled() {
        return Err(AuthError::Canceled);
    }
    let body = String::from_utf8_lossy(&bytes).into_owned();

    // The URL is redacted and the body is reported only by length. Both matter:
    // a device-code poll URL is harmless but a token response body is the
    // credential itself.
    debug!(
        stage = stage.as_str(),
        status,
        url = %redacted(url),
        bytes = body.len(),
        "authentication response"
    );

    Ok(RawResponse { status, body })
}

/// Parses a JSON document, attributing a syntax error to the right leg.
pub fn parse<T: DeserializeOwned>(stage: AuthStage, body: &str) -> AuthResult<T> {
    serde_json::from_str(body).map_err(|source| AuthError::malformed(stage, source))
}

/// Microsoft's OAuth error document, which every identity-platform endpoint
/// uses for failures.
///
/// `error_description` carries the `AADSTS…` code developers need and never
/// carries a token, so it is safe to surface. The correlation ids are dropped:
/// they identify a request to Microsoft's support, not to this user.
#[derive(Debug, Deserialize)]
pub struct OAuthErrorBody {
    pub error: String,
    #[serde(default)]
    pub error_description: String,
}

impl OAuthErrorBody {
    /// Turns the document into this crate's error.
    #[must_use]
    pub fn into_error(self) -> AuthError {
        match self.error.as_str() {
            // The refresh token is gone, revoked, or expired: Microsoft
            // token expired" once refreshing has itself failed.
            "invalid_grant" | "interaction_required" | "consent_required" => {
                AuthError::ReauthenticationRequired { code: self.error }
            }
            "authorization_declined" | "access_denied" => AuthError::Declined,
            "expired_token" => AuthError::DeviceCodeExpired,
            _ => AuthError::OAuth {
                code: self.error,
                description: self.error_description,
            },
        }
    }

    /// Like [`Self::into_error`], for a document that arrived with `status`.
    ///
    /// A code with no typed meaning says nothing about whether a retry could
    /// pass, so when the response status is transient the status decides and
    /// the failure stays retryable. Codes with a typed meaning keep it.
    #[must_use]
    pub fn into_error_for_status(self, stage: AuthStage, status: u16) -> AuthError {
        match self.into_error() {
            AuthError::OAuth { code, .. } if status_is_transient(status) => {
                debug!(
                    status,
                    code, "transient status kept over an unrecognized OAuth code"
                );
                AuthError::HttpStatus { stage, status }
            }
            error => error,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    /// Serves one response over raw TCP: `head` and `first` immediately, then
    /// `rest` once `release` fires. wiremock cannot pause mid-body.
    async fn serve_split(
        head: String,
        first: Vec<u8>,
        rest: Vec<u8>,
    ) -> (Url, oneshot::Sender<()>, oneshot::Receiver<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let (release, released) = oneshot::channel::<()>();
        let (sent_first, first_sent) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut scratch = [0_u8; 2048];
            let _ = stream.read(&mut scratch).await;
            stream.write_all(head.as_bytes()).await.unwrap();
            stream.write_all(&first).await.unwrap();
            stream.flush().await.unwrap();
            let _ = sent_first.send(());
            if released.await.is_ok() {
                let _ = stream.write_all(&rest).await;
                let _ = stream.flush().await;
            }
        });
        (url, release, first_sent)
    }

    fn client() -> Client {
        build_client(&HttpConfig::for_loopback_tests()).unwrap()
    }

    #[tokio::test]
    async fn cancellation_during_the_body_never_completes_successfully() {
        let (url, release, first_sent) = serve_split(
            "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n".to_owned(),
            b"{\"a\"".to_vec(),
            b":1}".to_vec(),
        )
        .await;
        let cancel = CancellationToken::new();
        let client = client();
        let mut task = {
            let (cancel, url) = (cancel.clone(), url.clone());
            tokio::spawn(async move {
                send(AuthStage::Microsoft, client.get(url.clone()), &url, &cancel).await
            })
        };
        first_sent.await.unwrap();
        // Let the client read the headers and the partial body, so the
        // cancellation below arrives while the body is being collected.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !task.is_finished(),
            "the stalled body should keep send pending"
        );

        cancel.cancel();
        // The server is still stalled: cancellation alone must end the call.
        let result = tokio::time::timeout(Duration::from_secs(5), &mut task)
            .await
            .expect("cancellation should end a stalled body read")
            .unwrap();
        assert!(matches!(result, Err(AuthError::Canceled)), "{result:?}");
        drop(release);
    }

    #[tokio::test]
    async fn a_body_over_the_ceiling_is_refused_when_content_length_is_declared() {
        let length = MAX_RESPONSE_BYTES + 1;
        let (url, release, _) = serve_split(
            format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"),
            Vec::new(),
            Vec::new(),
        )
        .await;
        let _ = release.send(());
        let result = send(
            AuthStage::Microsoft,
            client().get(url.clone()),
            &url,
            &CancellationToken::new(),
        )
        .await;
        assert!(
            matches!(result, Err(AuthError::ResponseTooLarge { .. })),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_body_over_the_ceiling_is_refused_when_content_length_is_absent() {
        let (url, release, _) = serve_split(
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_owned(),
            vec![b'x'; MAX_RESPONSE_BYTES + 1],
            Vec::new(),
        )
        .await;
        let _ = release.send(());
        let result = send(
            AuthStage::Microsoft,
            client().get(url.clone()),
            &url,
            &CancellationToken::new(),
        )
        .await;
        assert!(
            matches!(result, Err(AuthError::ResponseTooLarge { .. })),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_body_at_the_ceiling_is_delivered_intact() {
        let body = vec![b'x'; MAX_RESPONSE_BYTES];
        let (url, release, _) = serve_split(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {MAX_RESPONSE_BYTES}\r\nConnection: close\r\n\r\n"
            ),
            body,
            Vec::new(),
        )
        .await;
        let _ = release.send(());
        let response = send(
            AuthStage::Microsoft,
            client().get(url.clone()),
            &url,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body.len(), MAX_RESPONSE_BYTES);
    }

    #[test]
    fn an_oversized_response_is_not_worth_retrying() {
        let error = AuthError::ResponseTooLarge {
            stage: AuthStage::Microsoft,
            limit: MAX_RESPONSE_BYTES,
        };
        assert!(!error.is_transient());
        assert_eq!(error.message_id(), "error-auth-unexpected-response");
    }

    #[test]
    fn an_expired_refresh_token_asks_for_an_interactive_login() {
        let error = OAuthErrorBody {
            error: "invalid_grant".to_owned(),
            error_description: "AADSTS70000: The refresh token has expired.".to_owned(),
        }
        .into_error();
        assert!(error.requires_interactive_login(), "{error:?}");
    }

    #[test]
    fn an_unrecognized_oauth_code_on_a_transient_status_stays_transient() {
        for status in [429, 500, 503] {
            let error = OAuthErrorBody {
                error: "temporarily_unavailable".to_owned(),
                error_description: String::new(),
            }
            .into_error_for_status(AuthStage::Microsoft, status);
            assert!(
                matches!(error, AuthError::HttpStatus { status: s, .. } if s == status),
                "{status}: {error:?}"
            );
            assert!(error.is_transient(), "{status}");
        }
    }

    #[test]
    fn an_unrecognized_oauth_code_on_a_permanent_status_keeps_its_wording() {
        let error = OAuthErrorBody {
            error: "invalid_scope".to_owned(),
            error_description: "AADSTS70011: bad scope".to_owned(),
        }
        .into_error_for_status(AuthStage::Microsoft, 400);
        assert!(matches!(error, AuthError::OAuth { .. }), "{error:?}");
        assert!(!error.is_transient());
    }

    #[test]
    fn typed_oauth_codes_keep_their_meaning_on_a_transient_status() {
        let error = OAuthErrorBody {
            error: "invalid_grant".to_owned(),
            error_description: String::new(),
        }
        .into_error_for_status(AuthStage::Microsoft, 503);
        assert!(error.requires_interactive_login(), "{error:?}");

        let error = OAuthErrorBody {
            error: "access_denied".to_owned(),
            error_description: String::new(),
        }
        .into_error_for_status(AuthStage::Microsoft, 503);
        assert!(matches!(error, AuthError::Declined), "{error:?}");
    }

    #[test]
    fn a_declined_consent_prompt_is_not_a_fault() {
        for code in ["authorization_declined", "access_denied"] {
            let error = OAuthErrorBody {
                error: code.to_owned(),
                error_description: String::new(),
            }
            .into_error();
            assert!(matches!(error, AuthError::Declined), "{code}: {error:?}");
        }
    }

    #[test]
    fn an_unrecognized_oauth_code_keeps_microsofts_own_wording_for_the_log() {
        let error = OAuthErrorBody {
            error: "invalid_scope".to_owned(),
            error_description: "AADSTS70011: bad scope".to_owned(),
        }
        .into_error();
        match error {
            AuthError::OAuth { code, description } => {
                assert_eq!(code, "invalid_scope");
                assert!(description.contains("AADSTS70011"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn the_request_timeout_is_short_enough_to_be_a_failure_rather_than_a_hang() {
        assert!(REQUEST_TIMEOUT <= Duration::from_secs(60));
    }
}
