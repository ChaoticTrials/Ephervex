//! The HTTP plumbing shared by all five legs of the chain.
//!
//! # There is still only one HTTP client in the workspace
//!
//! `ephervex-download`'s [`build_client`] is what constructs it, so every
//! request made here inherits the same rustls stack, bounded redirect policy,
//! connect and read timeouts, and launcher `User-Agent` as every artifact
//! download. A bare `reqwest::Client::new()` in this crate
//! would opt out of all four at once, and the one that would go unnoticed
//! longest is the redirect bound.
//!
//! What this crate does *not* reuse is [`DownloadManager`], and the reason is
//! shape rather than principle: that type exists to stream a body to a path,
//! verify a hash, and finalize atomically. Authentication posts form and JSON
//! bodies and reads small JSON documents into memory; there is no destination
//! path, no publisher-supplied digest, and nothing to finalize. Forcing the
//! chain through it would mean writing OAuth tokens to disk in order to read
//! them back, which would expose secrets in user-visible diagnostics.
//!
//! [`build_client`]: ephervex_download::build_client
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

use ephervex_download::{HttpConfig, redacted};
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

/// Builds the HTTP client the chain uses.
pub fn build_client(config: &HttpConfig) -> AuthResult<Client> {
    ephervex_download::build_client(config).map_err(|source| AuthError::ClientBuild {
        source: Box::new(source),
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

/// Sends a request, honoring cancellation, and reads the whole body.
pub async fn send(
    stage: AuthStage,
    request: RequestBuilder,
    url: &Url,
    cancel: &CancellationToken,
) -> AuthResult<RawResponse> {
    let request = request.timeout(REQUEST_TIMEOUT);
    let response: Response = tokio::select! {
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
    let body = response
        .text()
        .await
        .map_err(|source| AuthError::transport(stage, source))?;

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
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

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
