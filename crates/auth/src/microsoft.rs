//! The Microsoft identity platform half of the chain: authorization code with
//! PKCE, the device authorization grant, and refresh.
//!
//! # Verified against Microsoft Learn on 2026-08-08
//!
//! * `POST {base}/consumers/oauth2/v2.0/devicecode` takes **`client_id` and
//!   `scope` only** - no `redirect_uri`, which is what makes the device flow
//!   work on a machine with no browser at all. It answers with `device_code`,
//!   `user_code`, `verification_uri`, `expires_in`, `interval`, and a
//!   human-readable `message`. `verification_uri_complete` is documented as
//!   *not* supported by Microsoft, so nothing here may depend on it.
//! * Polling is `grant_type=urn:ietf:params:oauth:grant-type:device_code` at the
//!   ordinary `/token` endpoint, and its four documented errors -
//!   `authorization_pending`, `authorization_declined`, `bad_verification_code`,
//!   `expired_token` - are the flow's normal control path rather than
//!   exceptions.
//! * The authorization request carries `code_challenge` and
//!   `code_challenge_method`; redemption carries `code_verifier`. A public
//!   client sends **no** `client_secret` in either leg, and Microsoft
//!   explicitly documents that a native app must not.
//! * A new `refresh_token` may come back from any redemption. Microsoft's own
//!   wording: "You're expected to discard the old refresh token." This module
//!   therefore always returns whatever it was given, and the caller always
//!   re-stores it.
//!
//! # Both flows need the same one-time Azure configuration
//!
//! Registering `http://localhost` under **Mobile and desktop applications** is
//! what the browser flow needs, and doing so is also what sets
//! `allowPublicClient`, which is what the device flow needs (without it the
//! token endpoint answers `AADSTS7000218`, demanding a `client_secret` a public
//! client cannot have). So the device flow costs no configuration the browser
//! flow has not already paid for, and both ship.
//!
//! `http://localhost` and not `http://127.0.0.1`, for two reasons that both
//! come from Microsoft's redirect-URI rules: the port is wildcarded only for
//! `localhost`, and an `http` loopback URI with the IP literal cannot be added
//! through the portal at all. The full argument, and the IPv6 problem that
//! choice creates and how it is solved, are in [`crate::loopback::REDIRECT_HOST`].

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::Client;
use tokio_util::sync::CancellationToken;
use tokio_util::sync::WaitForCancellationFuture;
use tracing::{debug, instrument};

use crate::endpoints::{Endpoints, SCOPE};
use crate::error::{AuthError, AuthResult};
use crate::http::{OAuthErrorBody, RawResponse, send};
use crate::pkce::Pkce;
use crate::secret::SecretString;
use crate::stage::AuthStage;

/// The device authorization grant's `grant_type`.
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Floor for the poll interval, in case a service ever reports `0`.
const MIN_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Ceiling for the poll interval, so a hostile or broken response cannot park
/// a login for an hour.
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// A Microsoft token pair.
///
/// `refresh` is optional because Microsoft only issues one when
/// `offline_access` was granted - which Ephervex always requests, but a
/// response is not a place to assume.
#[derive(Debug)]
pub struct MicrosoftTokens {
    pub access: SecretString,
    pub refresh: Option<SecretString>,
    pub expires_at: DateTime<Utc>,
}

/// What the user has to do to finish a device-code login.
///
/// Everything here is meant to be displayed. The `device_code` that goes with
/// it is not, and is held separately in [`DeviceCodeSession`].
#[derive(Debug, Clone)]
pub struct DeviceCodePrompt {
    /// The short code the user types.
    pub user_code: String,
    /// Where they type it, e.g. `https://microsoft.com/devicelogin`.
    pub verification_uri: String,
    /// Microsoft's own instruction sentence. Shown as-is: it is already
    /// localized by the service and re-wording it would drift from what the
    /// page the user lands on actually says.
    pub message: String,
    pub expires_at: DateTime<Utc>,
}

/// A device-code login in progress.
#[derive(Debug)]
pub struct DeviceCodeSession {
    pub prompt: DeviceCodePrompt,
    device_code: SecretString,
    interval: Duration,
}

/// The Microsoft identity platform client.
#[derive(Debug, Clone)]
pub struct MicrosoftClient {
    http: Client,
    endpoints: Endpoints,
    client_id: String,
}

impl MicrosoftClient {
    /// Builds a client for one Azure application registration.
    ///
    /// An empty client id is refused here rather than producing a confusing
    /// `AADSTS900023` from Microsoft three requests later.
    pub fn new(http: Client, endpoints: Endpoints, client_id: &str) -> AuthResult<Self> {
        let client_id = client_id.trim();
        if client_id.is_empty() {
            return Err(AuthError::ClientIdMissing);
        }
        Ok(Self {
            http,
            endpoints,
            client_id: client_id.to_owned(),
        })
    }

    /// The application registration this client authenticates as. It is not a
    /// secret, so it is safe to display and to log.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    // -- authorization code + PKCE ----------------------------------------

    /// Builds the URL to open in the user's browser.
    pub fn authorization_url(&self, pkce: &Pkce, redirect_uri: &str) -> AuthResult<String> {
        let mut url = self.endpoints.authorize()?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect_uri)
            // `query` rather than `fragment`: a fragment is never sent to the
            // server, so the loopback listener would never see the code.
            .append_pair("response_mode", "query")
            .append_pair("scope", SCOPE)
            .append_pair("state", pkce.state())
            .append_pair("code_challenge", pkce.challenge())
            .append_pair("code_challenge_method", pkce.challenge_method());
        Ok(url.to_string())
    }

    /// Redeems an authorization code, proving possession of the PKCE verifier.
    #[instrument(skip_all)]
    pub async fn redeem_code(
        &self,
        code: &SecretString,
        pkce: &Pkce,
        redirect_uri: &str,
        cancel: &CancellationToken,
    ) -> AuthResult<MicrosoftTokens> {
        self.token_request(
            &[
                ("client_id", self.client_id.as_str()),
                ("grant_type", "authorization_code"),
                ("code", code.expose()),
                ("redirect_uri", redirect_uri),
                ("code_verifier", pkce.verifier().expose()),
                ("scope", SCOPE),
            ],
            cancel,
        )
        .await
    }

    // -- device code -------------------------------------------------------

    /// Starts a device-code login and returns what to show the user.
    #[instrument(skip_all)]
    pub async fn begin_device_code(
        &self,
        cancel: &CancellationToken,
    ) -> AuthResult<DeviceCodeSession> {
        let url = self.endpoints.device_code()?;
        let request = self
            .http
            .post(url.clone())
            // Only these two. A `redirect_uri` here is not merely unnecessary,
            // it is not part of the request Microsoft documents.
            .form(&[("client_id", self.client_id.as_str()), ("scope", SCOPE)]);

        let response = send(AuthStage::Microsoft, request, &url, cancel).await?;
        if !response.is_success() {
            return Err(oauth_failure(&response, AuthStage::Microsoft));
        }

        let body: DeviceCodeBody = response.parse(AuthStage::Microsoft)?;
        let interval =
            Duration::from_secs(body.interval.max(1)).clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL);
        debug!(
            expires_in = body.expires_in,
            interval = interval.as_secs(),
            "device code issued"
        );

        Ok(DeviceCodeSession {
            prompt: DeviceCodePrompt {
                user_code: body.user_code,
                verification_uri: body.verification_uri,
                message: body.message,
                expires_at: expiry(body.expires_in),
            },
            device_code: body.device_code,
            interval,
        })
    }

    /// Polls until the user finishes, declines, or the code expires.
    ///
    /// `authorization_pending` and `slow_down` are the flow working correctly,
    /// not failures: the first means "keep waiting", the second additionally
    /// means "and back off", which RFC 8628 requires the client to honor by
    /// permanently increasing its interval.
    #[instrument(skip_all)]
    pub async fn poll_device_code(
        &self,
        session: &DeviceCodeSession,
        cancel: &CancellationToken,
    ) -> AuthResult<MicrosoftTokens> {
        let mut interval = session.interval;
        loop {
            wait(interval, cancel).await?;

            let response = self
                .post_token(
                    &[
                        ("client_id", self.client_id.as_str()),
                        ("grant_type", DEVICE_CODE_GRANT),
                        ("device_code", session.device_code.expose()),
                    ],
                    cancel,
                )
                .await?;

            if response.is_success() {
                return response
                    .parse::<TokenBody>(AuthStage::Microsoft)
                    .map(TokenBody::into_tokens);
            }

            let error: OAuthErrorBody = response.parse(AuthStage::Microsoft)?;
            match error.error.as_str() {
                "authorization_pending" => {}
                "slow_down" => {
                    // RFC 8628 section 3.5: the increase is permanent for the rest of
                    // this session, not a one-off pause.
                    interval = (interval + Duration::from_secs(5)).min(MAX_POLL_INTERVAL);
                    debug!(
                        interval = interval.as_secs(),
                        "device code poll backing off"
                    );
                }
                // `bad_verification_code` means the device code this process is
                // sending is not one the service recognizes. Retrying cannot
                // fix that, and a poll loop that never terminates is worse than
                // a clear failure.
                "bad_verification_code" => {
                    return Err(AuthError::OAuth {
                        code: error.error,
                        description: error.error_description,
                    });
                }
                _ => return Err(error.into_error()),
            }
        }
    }

    // -- refresh -----------------------------------------------------------

    /// Exchanges a refresh token for a fresh access token.
    ///
    /// The returned pair may carry a *new* refresh token, and the caller must
    /// store it: Microsoft may revoke the old one once a replacement is issued.
    #[instrument(skip_all)]
    pub async fn refresh(
        &self,
        refresh_token: &SecretString,
        cancel: &CancellationToken,
    ) -> AuthResult<MicrosoftTokens> {
        self.token_request(
            &[
                ("client_id", self.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token.expose()),
                ("scope", SCOPE),
            ],
            cancel,
        )
        .await
    }

    // -- shared ------------------------------------------------------------

    async fn token_request(
        &self,
        form: &[(&str, &str)],
        cancel: &CancellationToken,
    ) -> AuthResult<MicrosoftTokens> {
        let response = self.post_token(form, cancel).await?;
        if !response.is_success() {
            return Err(oauth_failure(&response, AuthStage::Microsoft));
        }
        response
            .parse::<TokenBody>(AuthStage::Microsoft)
            .map(TokenBody::into_tokens)
    }

    async fn post_token(
        &self,
        form: &[(&str, &str)],
        cancel: &CancellationToken,
    ) -> AuthResult<RawResponse> {
        let url = self.endpoints.token()?;
        // No `client_secret` and no `client_assertion`: this is a public client
        // and Microsoft documents that a native app must not send one.
        let request = self.http.post(url.clone()).form(form);
        send(AuthStage::Microsoft, request, &url, cancel).await
    }
}

/// Turns a non-2xx token response into an error, preferring Microsoft's own
/// OAuth document over the bare status when one is present.
fn oauth_failure(response: &RawResponse, stage: AuthStage) -> AuthError {
    response.parse::<OAuthErrorBody>(stage).map_or_else(
        |_| AuthError::HttpStatus {
            stage,
            status: response.status,
        },
        OAuthErrorBody::into_error,
    )
}

/// Sleeps, unless the caller cancels first.
async fn wait(duration: Duration, cancel: &CancellationToken) -> AuthResult<()> {
    let canceled: WaitForCancellationFuture<'_> = cancel.cancelled();
    tokio::select! {
        biased;
        () = canceled => Err(AuthError::Canceled),
        () = tokio::time::sleep(duration) => Ok(()),
    }
}

/// `expires_in` seconds from now.
fn expiry(seconds: i64) -> DateTime<Utc> {
    Utc::now() + chrono::TimeDelta::try_seconds(seconds).unwrap_or_default()
}

#[derive(Debug, serde::Deserialize)]
struct TokenBody {
    access_token: SecretString,
    #[serde(default)]
    refresh_token: Option<SecretString>,
    #[serde(default)]
    expires_in: i64,
}

impl TokenBody {
    fn into_tokens(self) -> MicrosoftTokens {
        MicrosoftTokens {
            access: self.access_token,
            // An empty string is not a refresh token; treating it as one would
            // store a credential that fails on the next launch.
            refresh: self.refresh_token.filter(|token| !token.is_empty()),
            expires_at: expiry(self.expires_in),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct DeviceCodeBody {
    device_code: SecretString,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    expires_in: i64,
    #[serde(default)]
    interval: u64,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use reqwest::Url;

    use super::*;

    fn client() -> MicrosoftClient {
        MicrosoftClient::new(
            Client::new(),
            Endpoints::production(),
            "767ea465-0bba-443b-a06b-831c71777b90",
        )
        .expect("a client id is configured")
    }

    #[test]
    fn an_empty_client_id_is_refused_before_a_request_is_made() {
        let error = MicrosoftClient::new(Client::new(), Endpoints::production(), "   ")
            .expect_err("must refuse");
        assert!(matches!(error, AuthError::ClientIdMissing));
    }

    #[test]
    fn the_authorization_url_carries_pkce_and_no_secret() {
        let pkce = Pkce::generate().expect("entropy");
        let url = client()
            .authorization_url(&pkce, "http://localhost:41234")
            .expect("url");
        let parsed = Url::parse(&url).expect("parses");
        let query: std::collections::HashMap<_, _> = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        assert_eq!(query["response_type"], "code");
        // `fragment` never reaches the server, so the loopback listener would
        // never see the code.
        assert_eq!(query["response_mode"], "query");
        // `localhost`, not the IP literal: Microsoft ignores the port when
        // matching only for `localhost`. See `loopback::REDIRECT_HOST`.
        assert_eq!(query["redirect_uri"], "http://localhost:41234");
        assert_eq!(query["code_challenge"], pkce.challenge());
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["state"], pkce.state());
        assert_eq!(query["scope"], SCOPE);
        assert!(
            !query.contains_key("client_secret"),
            "a public client has no secret to send"
        );
        // The verifier stays in this process; sending it here would defeat PKCE
        // entirely.
        assert!(!url.contains(pkce.verifier().expose()));
    }

    #[test]
    fn the_authorization_url_points_at_the_consumers_tenant() {
        let pkce = Pkce::generate().expect("entropy");
        let url = client()
            .authorization_url(&pkce, "http://localhost:1")
            .unwrap();
        assert!(
            url.starts_with("https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize?")
        );
    }

    #[test]
    fn a_token_response_without_a_refresh_token_reports_none_rather_than_empty() {
        let body: TokenBody = serde_json::from_str(
            r#"{"token_type":"Bearer","expires_in":3599,"access_token":"eyJ0eXAi"}"#,
        )
        .expect("parses");
        let tokens = body.into_tokens();
        assert!(tokens.refresh.is_none());
        assert!(tokens.expires_at > Utc::now());

        let body: TokenBody =
            serde_json::from_str(r#"{"access_token":"a","refresh_token":"","expires_in":1}"#)
                .expect("parses");
        assert!(
            body.into_tokens().refresh.is_none(),
            "an empty string is not a credential"
        );
    }

    #[test]
    fn a_device_code_response_parses_into_a_prompt_and_a_hidden_code() {
        // Shape verified against Microsoft Learn on 2026-08-08.
        let body: DeviceCodeBody = serde_json::from_str(
            r#"{
                "device_code": "GMMhmHCXhWEzkobqIHGG_EnNYYsAkukHspeYUk9E8",
                "user_code": "A1B2C3D4",
                "verification_uri": "https://microsoft.com/devicelogin",
                "expires_in": 900,
                "interval": 5,
                "message": "To sign in, use a web browser to open the page..."
            }"#,
        )
        .expect("parses");

        assert_eq!(body.user_code, "A1B2C3D4");
        assert_eq!(body.interval, 5);
        assert_eq!(body.expires_in, 900, "Microsoft's documented 15 minutes");
        // The device code is a bearer credential for the poll and must not be
        // renderable.
        assert!(!format!("{body:?}").contains("GMMhmHCXhWEz"));
    }

    #[test]
    fn a_zero_or_absent_poll_interval_is_clamped_rather_than_busy_looping() {
        let clamp = |seconds: u64| {
            Duration::from_secs(seconds.max(1)).clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL)
        };
        assert_eq!(clamp(0), MIN_POLL_INTERVAL);
        assert_eq!(clamp(5), Duration::from_secs(5));
        assert_eq!(clamp(3600), MAX_POLL_INTERVAL);
    }
}
