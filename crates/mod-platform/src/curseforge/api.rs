//! Authenticated requests against the CurseForge third-party REST API.
//!
//! # One header, attached per request
//!
//! The API authenticates with `x-api-key`. It is set on each request rather
//! than installed as a default header on the client, so the credential cannot
//! be inherited by some later caller that happens to share a connection pool.
//! It is also marked sensitive, which keeps it out of the HTTP/2 header index.
//!
//! `x-api-key` is not `X-Api-Token`. The latter belongs to the CurseForge
//! Upload API, which is a different service with a different credential, and
//! is not what this module speaks to.
//!
//! # Nothing is written to disk
//!
//! There is no [`MetadataCache`](ephervex_download::MetadataCache) here and no
//! staging path. A response is read into memory, parsed, and dropped. See the
//! module documentation on [`super`] for why that is a requirement rather than
//! an omission.

use std::sync::Arc;
use std::time::Duration;

use ephervex_download::{HttpConfig, Url, build_single_origin_client, check_transport, redacted};
use ephervex_model::ModProvider;
use reqwest::header::{ACCEPT, HeaderName, HeaderValue};
use reqwest::{Client, Response, StatusCode};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::credential::{CurseForgeCredentialSource, missing};
use super::dto::{ApiEnvelope, CurseForgeGame, CurseForgeProject};
use crate::error::{ModPlatformError, ModPlatformResult};

/// The production API root.
pub const CURSEFORGE_API_BASE: &str = "https://api.curseforge.com/v1";

/// CurseForge's game id for Minecraft.
///
/// A published, stable identifier: every Minecraft query on this API carries
/// it, and it is what makes a lookup mean "Minecraft" rather than "some game".
pub const MINECRAFT_GAME_ID: u32 = 432;

/// The header the third-party API authenticates with.
const API_KEY_HEADER: &str = "x-api-key";

/// How long a single API request may take.
///
/// Short, like authentication's: nothing this client asks for is more than a
/// few kilobytes, and a user waiting on a settings screen deserves a failure
/// rather than a spinner.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The CurseForge third-party API, as much of it as this launcher uses.
#[derive(Debug, Clone)]
pub struct CurseForgeApi {
    client: Client,
    base: Url,
    credential: Arc<dyn CurseForgeCredentialSource>,
}

impl CurseForgeApi {
    /// A client against the production API.
    pub fn new(
        credential: Arc<dyn CurseForgeCredentialSource>,
        http: &HttpConfig,
    ) -> ModPlatformResult<Self> {
        Self::with_base_url(credential, http, CURSEFORGE_API_BASE)
    }

    /// A client against an explicit API root.
    ///
    /// The root is a parameter so integration tests can point the client at a
    /// loopback server, the same way the Modrinth provider does. Production
    /// always uses [`CURSEFORGE_API_BASE`], and the transport check below still
    /// refuses plaintext HTTP unless a test has explicitly opted in.
    ///
    /// The origin of this root is also the boundary the credential may travel
    /// to: redirects that leave it are refused rather than followed.
    pub fn with_base_url(
        credential: Arc<dyn CurseForgeCredentialSource>,
        http: &HttpConfig,
        base: &str,
    ) -> ModPlatformResult<Self> {
        let base = Url::parse(&format!("{}/", base.trim_end_matches('/')))
            .map_err(|error| unusable(&error.to_string()))?;
        check_transport(&base, http.allow_plaintext_http)
            .map_err(|error| unusable(&error.to_string()))?;
        let client = build_single_origin_client(http, &base)
            .map_err(|error| unusable(&error.to_string()))?;
        Ok(Self {
            client,
            base,
            credential,
        })
    }

    /// The API root this client talks to.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// Whether an API key is configured at all.
    ///
    /// Answered without a network request, because the settings screen that
    /// asks is not allowed to need one. Reports the *presence* of a credential
    /// and never its value.
    pub fn has_credential(&self) -> ModPlatformResult<bool> {
        Ok(self.credential.api_key()?.is_some())
    }

    /// One game, by id.
    ///
    /// Small and cheap, and the reason it exists is explicit: it is the
    /// smallest authenticated call that proves a configured key is accepted.
    /// Asking for [`MINECRAFT_GAME_ID`] is a credential check, not the start of
    /// a catalog — nothing it returns is kept.
    pub async fn game(
        &self,
        game_id: u32,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<CurseForgeGame> {
        let url = self.endpoint(&["games", &game_id.to_string()])?;
        self.get::<CurseForgeGame>(&url, cancel).await
    }

    /// One project, by its numeric CurseForge id.
    pub async fn project(
        &self,
        project_id: u32,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<CurseForgeProject> {
        let url = self.endpoint(&["mods", &project_id.to_string()])?;
        match self.get::<CurseForgeProject>(&url, cancel).await {
            // A 404 from this endpoint means the id names nothing, which is
            // worth saying rather than reporting as an outage.
            Err(ModPlatformError::HttpStatus { status, .. }) if status == StatusCode::NOT_FOUND => {
                Err(ModPlatformError::ProjectUnknown {
                    provider: ModProvider::CurseForge,
                    reference: project_id.to_string(),
                })
            }
            other => other,
        }
    }

    /// Builds an endpoint URL from path segments.
    ///
    /// Every segment goes through `Url`'s own escaping. Nothing a user typed
    /// reaches one today — both endpoints take a number — and routing them
    /// through here is what keeps that true when one takes a slug.
    fn endpoint(&self, segments: &[&str]) -> ModPlatformResult<Url> {
        let mut url = self.base.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|()| unusable("the api base cannot have path segments"))?;
            // The base is normalized with a trailing slash, which leaves an
            // empty final segment; appending onto it would produce `//`.
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(url)
    }

    /// Performs one authenticated GET and reads the answer.
    ///
    /// The credential is resolved *first*: a launcher with no key configured
    /// must fail without contacting CurseForge at all, both because the request
    /// could only be refused and because an unauthenticated call still spends
    /// somebody's rate limit.
    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        url: &Url,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<T> {
        let key = self.credential.api_key()?.ok_or_else(missing)?;
        let mut header = HeaderValue::from_str(key.expose()).map_err(|_| {
            // A fixed sentence, never the rejected value.
            ModPlatformError::CredentialUnreadable {
                provider: ModProvider::CurseForge,
                detail: "the configured API key cannot be sent in a request header".to_owned(),
            }
        })?;
        // Keeps the value out of the HTTP/2 header index, where it would
        // otherwise be retained for the life of the connection.
        header.set_sensitive(true);

        let request = self
            .client
            .get(url.clone())
            .header(HeaderName::from_static(API_KEY_HEADER), header)
            .header(ACCEPT, HeaderValue::from_static("application/json"))
            .timeout(REQUEST_TIMEOUT);

        let response: Response = tokio::select! {
            // Biased so a request canceled before it starts wins rather than
            // racing a round trip whose answer is going to be discarded.
            biased;
            () = cancel.cancelled() => return Err(ModPlatformError::Canceled),
            result = request.send() => result.map_err(|error| request_failed(&error))?,
        };

        let status = response.status();
        // The address without its query, the status, and nothing else. Never
        // the request headers: one of them is the credential.
        debug!(
            status = status.as_u16(),
            url = %redacted(url),
            "curseforge api response"
        );
        classify(status)?;

        let body = response
            .text()
            .await
            .map_err(|error| request_failed(&error))?;
        serde_json::from_str::<ApiEnvelope<T>>(&body)
            .map(|envelope| envelope.data)
            .map_err(|error| ModPlatformError::ResponseInvalid {
                provider: ModProvider::CurseForge,
                detail: error.to_string(),
            })
    }
}

/// Turns a non-success status into the failure it means.
///
/// `401` and `403` are the two the user can act on, and they get the sentence
/// that says so. Everything else — including the `3xx` that a refused
/// cross-origin redirect leaves behind — is a status this launcher cannot use.
fn classify(status: StatusCode) -> ModPlatformResult<()> {
    if status.is_success() {
        return Ok(());
    }
    let provider = ModProvider::CurseForge;
    let status = status.as_u16();
    Err(
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            ModPlatformError::CredentialRejected { provider, status }
        } else {
            ModPlatformError::HttpStatus { provider, status }
        },
    )
}

/// Turns a transport failure into this crate's own.
///
/// `reqwest`'s own message carries the address it was working on, which is
/// wanted, and never a request header, which is why it is safe to keep.
fn request_failed(source: &reqwest::Error) -> ModPlatformError {
    ModPlatformError::RequestFailed {
        provider: ModProvider::CurseForge,
        detail: source.to_string(),
    }
}

/// A base URL or client this build cannot make requests with.
fn unusable(detail: &str) -> ModPlatformError {
    ModPlatformError::ResponseInvalid {
        provider: ModProvider::CurseForge,
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::SecretString;

    use super::super::credential::ConfiguredApiKey;
    use super::*;

    const FAKE_KEY: &str = "test-curseforge-key-do-not-use";

    fn api(base: &str) -> CurseForgeApi {
        CurseForgeApi::with_base_url(
            Arc::new(ConfiguredApiKey::new(SecretString::new(FAKE_KEY))),
            &HttpConfig::for_loopback_tests(),
            base,
        )
        .expect("a client builds")
    }

    #[test]
    fn the_production_root_is_the_documented_third_party_api() {
        assert_eq!(CURSEFORGE_API_BASE, "https://api.curseforge.com/v1");
        assert_eq!(MINECRAFT_GAME_ID, 432);
    }

    #[test]
    fn the_credential_header_is_the_third_party_api_key_and_not_the_upload_token() {
        // Two different credential systems for two different services. Sending
        // one where the other belongs fails in a way nobody can diagnose.
        assert_eq!(API_KEY_HEADER, "x-api-key");
        assert_ne!(API_KEY_HEADER.to_ascii_lowercase(), "x-api-token");
    }

    #[test]
    fn endpoints_are_built_under_the_configured_root() {
        let client = api("http://127.0.0.1:9/v1");
        assert_eq!(
            client.endpoint(&["games", "432"]).expect("a url").as_str(),
            "http://127.0.0.1:9/v1/games/432"
        );
        assert_eq!(
            client
                .endpoint(&["mods", "238222"])
                .expect("a url")
                .as_str(),
            "http://127.0.0.1:9/v1/mods/238222"
        );
        // A trailing slash on the root must not double up in the path.
        assert_eq!(
            api("http://127.0.0.1:9/v1/")
                .endpoint(&["games", "432"])
                .expect("a url")
                .as_str(),
            "http://127.0.0.1:9/v1/games/432"
        );
    }

    #[test]
    fn no_endpoint_puts_the_key_in_the_address() {
        // The key is a header. A query parameter would end up in server logs,
        // in browser history if the address were ever opened, and in any
        // referrer the API happened to send.
        let url = api("http://127.0.0.1:9/v1")
            .endpoint(&["games", "432"])
            .expect("a url");
        assert!(url.query().is_none());
        assert!(!url.as_str().contains(FAKE_KEY));
    }

    #[test]
    fn a_plaintext_root_is_refused_unless_a_test_has_opted_in() {
        let error = CurseForgeApi::with_base_url(
            Arc::new(ConfiguredApiKey::absent()),
            &HttpConfig::default(),
            "http://api.curseforge.com/v1",
        )
        .expect_err("production refuses plaintext");
        assert!(matches!(error, ModPlatformError::ResponseInvalid { .. }));
    }

    #[test]
    fn the_presence_of_a_key_is_answered_without_a_request() {
        // The settings screen asks this, and saving a credential must never
        // depend on being online.
        assert!(
            api("https://api.curseforge.com/v1")
                .has_credential()
                .expect("read")
        );
        let unconfigured =
            CurseForgeApi::new(Arc::new(ConfiguredApiKey::absent()), &HttpConfig::default())
                .expect("a client builds");
        assert!(!unconfigured.has_credential().expect("read"));
    }

    #[test]
    fn statuses_are_classified_into_what_the_user_can_do_about_them() {
        assert!(classify(StatusCode::OK).is_ok());
        for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
            let error = classify(status).expect_err("a refusal");
            assert!(error.is_credential_problem(), "{error:?}");
        }
        for status in [
            StatusCode::NOT_FOUND,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            // What a refused cross-origin redirect leaves behind.
            StatusCode::FOUND,
        ] {
            let error = classify(status).expect_err("a refusal");
            assert!(!error.is_credential_problem(), "{status}: {error:?}");
            assert!(matches!(error, ModPlatformError::HttpStatus { .. }));
        }
    }

    #[tokio::test]
    async fn a_request_canceled_before_it_starts_never_reaches_the_network() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        // Port 9 discards everything, so anything that did reach the network
        // would fail with a transport error rather than a cancellation.
        let error = api("http://127.0.0.1:9/v1")
            .game(MINECRAFT_GAME_ID, &cancel)
            .await
            .expect_err("canceled");
        assert!(error.is_cancellation(), "{error:?}");
    }

    #[tokio::test]
    async fn a_missing_key_fails_before_any_request_is_attempted() {
        let api = CurseForgeApi::with_base_url(
            Arc::new(ConfiguredApiKey::absent()),
            &HttpConfig::for_loopback_tests(),
            "http://127.0.0.1:9/v1",
        )
        .expect("a client builds");

        let error = api
            .game(MINECRAFT_GAME_ID, &CancellationToken::new())
            .await
            .expect_err("no key is configured");
        // A transport failure here would mean the request was made anyway.
        assert!(
            matches!(error, ModPlatformError::CredentialMissing { .. }),
            "{error:?}"
        );
    }
}
