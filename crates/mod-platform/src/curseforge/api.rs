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

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use ephervex_download::{HttpConfig, Url, build_single_origin_client, check_transport, redacted};
use ephervex_model::{
    CurseForgeFileId, CurseForgeProjectId, Hash, HashAlgorithm, ModProvider, SecretString,
};
use reqwest::header::{ACCEPT, HeaderName, HeaderValue};
use reqwest::{Client, Response, StatusCode};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::credential::{CurseForgeCredentialSource, missing};
use super::dto::{
    ApiEnvelope, CurseForgeFileDocument, CurseForgeFileHash, CurseForgeGame, CurseForgeProject,
    HASH_ALGO_SHA1,
};
use crate::error::{ModPlatformError, ModPlatformResult};
use crate::model::ModFile;

/// The production API root.
pub const CURSEFORGE_API_BASE: &str = "https://api.curseforge.com/v1";

/// CurseForge's game id for Minecraft.
///
/// A published, stable identifier: every Minecraft query on this API carries
/// it, and it is what makes a lookup mean "Minecraft" rather than "some game".
pub const MINECRAFT_GAME_ID: u32 = 432;

/// The header the third-party API authenticates with.
///
/// Public because the same header is what an authenticated *artifact* request
/// carries, and that request is built above this crate. One definition means
/// the two cannot drift into sending different headers for one credential.
pub const CURSEFORGE_API_KEY_HEADER: &str = "x-api-key";

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

    /// Holds this operation's credential for its sequence of requests.
    ///
    /// A pack import asks for hundreds of files, and reading the secret store
    /// once per request would mean hundreds of keyring round trips over D-Bus
    /// for one import. The scope reads it once and drops it when the operation
    /// ends — deliberately not longer: the credential's lifetime is the
    /// operation's, and holding one on the client so a later operation could
    /// reuse it is exactly the extension this avoids.
    ///
    /// Resolving it up front also means an import with no key configured fails
    /// before contacting CurseForge at all, both because the requests could
    /// only be refused and because unauthenticated calls still spend somebody's
    /// rate limit.
    pub fn begin_operation(&self) -> ModPlatformResult<CurseForgeOperation<'_>> {
        let key = self.credential.api_key()?.ok_or_else(missing)?;
        Ok(CurseForgeOperation { api: self, key })
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
        self.get_with_key(url, &key, cancel).await
    }

    /// The same request with the credential already in hand.
    async fn get_with_key<T: serde::de::DeserializeOwned>(
        &self,
        url: &Url,
        key: &SecretString,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<T> {
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
            .header(HeaderName::from_static(CURSEFORGE_API_KEY_HEADER), header)
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

/// One operation's sequence of authenticated CurseForge requests.
///
/// Exists for two reasons, both about the credential. It is read once and lives
/// exactly as long as the operation that needs it, and it is available to
/// whatever else that operation authenticates — an artifact download, for
/// instance — without a second trip to the secret store.
///
/// Nothing it fetches is written down. See [`super`] for why that is a
/// requirement rather than an omission.
pub struct CurseForgeOperation<'a> {
    api: &'a CurseForgeApi,
    key: SecretString,
}

impl fmt::Debug for CurseForgeOperation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Names what this is and never what it holds. A derived implementation
        // would render the secret's own `Debug`, which is redacted — but the
        // guarantee should not depend on that staying true.
        formatter.write_str("CurseForgeOperation")
    }
}

impl CurseForgeOperation<'_> {
    /// The credential this operation may spend, for a request it makes itself.
    ///
    /// Handing out the secret is the point: an artifact download is part of the
    /// same operation and must not read the secret store again. What it may be
    /// sent to is the caller's to constrain.
    #[must_use]
    pub fn credential(&self) -> &SecretString {
        &self.key
    }

    /// One exact file of one exact project.
    ///
    /// `GET /v1/mods/{modId}/files/{fileId}` — the endpoint that answers about
    /// the file that was asked for. Deliberately not a version listing filtered
    /// down afterwards: a pack declares one file id, and anything that picks
    /// from a list can pick a newer one.
    ///
    /// The answer is checked against the question. A response describing
    /// another project or another file is refused rather than used, because a
    /// substituted artifact is precisely the failure an exact import exists to
    /// prevent.
    pub async fn file(
        &self,
        project: CurseForgeProjectId,
        file: CurseForgeFileId,
        cancel: &CancellationToken,
    ) -> ModPlatformResult<CurseForgeFileRelease> {
        let url = self
            .api
            .endpoint(&["mods", &project.to_string(), "files", &file.to_string()])?;
        let document: CurseForgeFileDocument =
            match self.api.get_with_key(&url, &self.key, cancel).await {
                Ok(document) => document,
                // A 404 here means this project has no such file, which is a fact
                // about the pack rather than an outage.
                Err(ModPlatformError::HttpStatus { status, .. })
                    if status == StatusCode::NOT_FOUND =>
                {
                    return Err(ModPlatformError::FileUnknown {
                        provider: ModProvider::CurseForge,
                        project_id: project.to_string(),
                        file_id: file.to_string(),
                    });
                }
                Err(other) => return Err(other),
            };
        normalize_file(project, file, document)
    }
}

/// One exact CurseForge file, normalized and checked against what was asked
/// for.
///
/// The provider's wire shape stops here: everything above works from
/// [`ModFile`] plus the two ids that name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurseForgeFileRelease {
    pub project: CurseForgeProjectId,
    pub file: CurseForgeFileId,
    /// The name, address, size, and verifiable digests, in the provider-neutral
    /// shape the rest of the launcher installs from.
    pub artifact: ModFile,
}

/// Checks one file document against the request and normalizes it.
///
/// Every refusal here is a distinct thing a user can be told and, where it is
/// their pack's problem rather than a fault, act on.
fn normalize_file(
    project: CurseForgeProjectId,
    file: CurseForgeFileId,
    document: CurseForgeFileDocument,
) -> ModPlatformResult<CurseForgeFileRelease> {
    if document.mod_id != project.get() || document.id != file.get() {
        return Err(ModPlatformError::FileIdentityMismatch {
            provider: ModProvider::CurseForge,
            requested: format!("{project}/{file}"),
            returned: format!("{}/{}", document.mod_id, document.id),
        });
    }
    if !document.is_available {
        return Err(ModPlatformError::FileUnavailable {
            provider: ModProvider::CurseForge,
            project_id: project.to_string(),
            file_id: file.to_string(),
        });
    }

    // Null rather than absent, and it means the project's owner turned off
    // third-party distribution. There is no second address to try: assembling a
    // CDN path out of the ids would route around a decision the author made.
    let url = document
        .download_url
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
        .ok_or_else(|| ModPlatformError::FileDistributionRestricted {
            provider: ModProvider::CurseForge,
            project_id: project.to_string(),
            file_id: file.to_string(),
        })?;

    let hashes = supported_hashes(&document.hashes);
    if hashes.is_empty() {
        // Verification is mandatory. A file with no digest this build can check
        // is one it will not install, rather than one it trusts because of the
        // address it came from.
        return Err(ModPlatformError::HashesMissing {
            version_id: file.to_string(),
        });
    }

    Ok(CurseForgeFileRelease {
        project,
        file,
        artifact: ModFile {
            file_name: document.file_name,
            url,
            size: document.file_length,
            hashes,
        },
    })
}

/// The digests from a file document this build can verify with.
///
/// SHA-1 only, because it is the one cryptographic hash CurseForge publishes
/// that Ephervex's hash model has. MD5 is published beside it and is not used:
/// it is not stronger, and a weaker digest is not a substitute for one that is
/// present. The fingerprint CurseForge also publishes is a lookup key rather
/// than a cryptographic hash and is not read at all.
///
/// A digest whose tag and shape disagree is dropped rather than trusted:
/// `Hash::new` enforces the hex length the algorithm has.
fn supported_hashes(published: &[CurseForgeFileHash]) -> Vec<Hash> {
    published
        .iter()
        .filter(|hash| hash.algo == HASH_ALGO_SHA1)
        .filter_map(|hash| Hash::new(HashAlgorithm::Sha1, hash.value.trim()))
        .collect()
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
        assert_eq!(CURSEFORGE_API_KEY_HEADER, "x-api-key");
        assert_ne!(
            CURSEFORGE_API_KEY_HEADER.to_ascii_lowercase(),
            "x-api-token"
        );
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

    // -- the exact-file endpoint -------------------------------------------

    fn project() -> CurseForgeProjectId {
        CurseForgeProjectId::new(238_222).expect("a real id")
    }

    fn file_id() -> CurseForgeFileId {
        CurseForgeFileId::new(4_593_548).expect("a real id")
    }

    fn document() -> CurseForgeFileDocument {
        serde_json::from_str(
            r#"{"id":4593548,"gameId":432,"modId":238222,"isAvailable":true,
                "fileName":"jei-1.20.1-15.2.0.27.jar","fileLength":1234567,
                "downloadUrl":"https://mediafilez.forgecdn.net/files/4593/548/jei.jar",
                "hashes":[{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1},
                          {"value":"0123456789abcdef0123456789abcdef","algo":2}]}"#,
        )
        .expect("a documented file response")
    }

    #[test]
    fn the_exact_file_endpoint_addresses_the_project_and_the_file_it_was_given() {
        assert_eq!(
            api("http://127.0.0.1:9/v1")
                .endpoint(&["mods", "238222", "files", "4593548"])
                .expect("a url")
                .as_str(),
            "http://127.0.0.1:9/v1/mods/238222/files/4593548"
        );
    }

    #[test]
    fn a_file_document_normalizes_to_the_address_size_and_digest_a_download_needs() {
        let release = normalize_file(project(), file_id(), document()).expect("a usable file");
        assert_eq!(release.project, project());
        assert_eq!(release.file, file_id());
        assert_eq!(release.artifact.file_name, "jei-1.20.1-15.2.0.27.jar");
        assert_eq!(
            release.artifact.url,
            "https://mediafilez.forgecdn.net/files/4593/548/jei.jar"
        );
        assert_eq!(release.artifact.size, Some(1_234_567));
    }

    #[test]
    fn only_the_cryptographic_digest_this_build_can_verify_is_kept() {
        // MD5 is published beside SHA-1 and is not stronger. Nothing is
        // recorded from it, and the fingerprint is not read at all.
        let release = normalize_file(project(), file_id(), document()).expect("a usable file");
        let algorithms: Vec<HashAlgorithm> = release
            .artifact
            .hashes
            .iter()
            .map(|hash| hash.algorithm)
            .collect();
        assert_eq!(algorithms, vec![HashAlgorithm::Sha1]);
        assert_eq!(
            release.artifact.hashes[0].digest(),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
    }

    #[test]
    fn a_digest_whose_tag_and_shape_disagree_is_dropped_rather_than_trusted() {
        let mut document = document();
        // Tagged SHA-1, sized like an MD5.
        document.hashes = vec![CurseForgeFileHash {
            value: "0123456789abcdef0123456789abcdef".to_owned(),
            algo: HASH_ALGO_SHA1,
        }];
        let error = normalize_file(project(), file_id(), document).expect_err("must refuse");
        assert!(matches!(error, ModPlatformError::HashesMissing { .. }));
    }

    #[test]
    fn a_file_with_no_verifiable_digest_is_refused_rather_than_trusted_by_address() {
        let mut document = document();
        document.hashes.retain(|hash| hash.algo != HASH_ALGO_SHA1);
        let error = normalize_file(project(), file_id(), document).expect_err("must refuse");
        assert!(
            matches!(error, ModPlatformError::HashesMissing { ref version_id } if version_id == "4593548"),
            "{error:?}"
        );
    }

    #[test]
    fn a_response_about_another_file_of_the_same_project_is_never_substituted() {
        // The whole point of an exact import: project 238222 file 4593548 means
        // that file, not the newest one the project happens to have.
        let mut newer = document();
        newer.id = 4_593_549;
        let error = normalize_file(project(), file_id(), newer).expect_err("must refuse");
        assert!(
            matches!(
                error,
                ModPlatformError::FileIdentityMismatch { ref requested, ref returned, .. }
                    if requested == "238222/4593548" && returned == "238222/4593549"
            ),
            "{error:?}"
        );
    }

    #[test]
    fn a_response_about_another_project_is_refused_too() {
        let mut elsewhere = document();
        elsewhere.mod_id = 999;
        let error = normalize_file(project(), file_id(), elsewhere).expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::FileIdentityMismatch { .. }
        ));
    }

    #[test]
    fn an_unavailable_file_and_a_restricted_one_are_told_apart() {
        let mut unavailable = document();
        unavailable.is_available = false;
        assert!(matches!(
            normalize_file(project(), file_id(), unavailable).expect_err("must refuse"),
            ModPlatformError::FileUnavailable { .. }
        ));

        let mut restricted = document();
        restricted.download_url = None;
        assert!(matches!(
            normalize_file(project(), file_id(), restricted).expect_err("must refuse"),
            ModPlatformError::FileDistributionRestricted { .. }
        ));

        let mut blank = document();
        blank.download_url = Some("   ".to_owned());
        assert!(
            matches!(
                normalize_file(project(), file_id(), blank).expect_err("must refuse"),
                ModPlatformError::FileDistributionRestricted { .. }
            ),
            "an empty address is no address"
        );
    }

    #[tokio::test]
    async fn an_operation_reads_the_credential_once_and_never_renders_it() {
        let client = api("http://127.0.0.1:9/v1");
        let operation = client.begin_operation().expect("a configured key");
        assert_eq!(operation.credential().expose(), FAKE_KEY);
        assert!(
            !format!("{operation:?}").contains(FAKE_KEY),
            "{operation:?}"
        );
    }

    #[tokio::test]
    async fn an_operation_cannot_begin_without_a_key_so_no_request_is_ever_made() {
        let api = CurseForgeApi::with_base_url(
            Arc::new(ConfiguredApiKey::absent()),
            &HttpConfig::for_loopback_tests(),
            "http://127.0.0.1:9/v1",
        )
        .expect("a client builds");
        assert!(matches!(
            api.begin_operation().expect_err("no key is configured"),
            ModPlatformError::CredentialMissing { .. }
        ));
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
