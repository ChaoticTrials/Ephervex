//! The CurseForge API client against loopback servers.
//!
//! Everything here runs against `wiremock` on 127.0.0.1 with an obviously fake
//! key. No test contacts CurseForge, none reads a credential from the
//! environment or the system keyring, and none needs a real API key to run.
//!
//! What these cover that the unit tests cannot: the request that is actually
//! sent, what happens to the credential when a response redirects, and the fact
//! that a completed request leaves nothing behind on disk.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use ephervex_download::{DomainAllowlist, HttpConfig};
use ephervex_mod_platform::{
    ConfiguredApiKey, CurseForgeApi, CurseForgeCredentialSource, MINECRAFT_GAME_ID, ModDependency,
    ModPlatformError, SideSupport,
};
use ephervex_model::{
    CurseForgeFileId, CurseForgeProjectId, Hash, HashAlgorithm, ModProvider, ModRelationKind,
    SecretString,
};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Obviously not a credential, and the only one this file ever uses.
const FAKE_KEY: &str = "test-curseforge-key-do-not-use";

/// The header the third-party API authenticates with.
const API_KEY_HEADER: &str = "x-api-key";

/// A documented game document, as `GET /v1/games/{gameId}` answers.
const MINECRAFT_GAME: &str =
    r#"{"data":{"id":432,"name":"Minecraft","slug":"minecraft","status":1}}"#;

fn api_against(
    server: &MockServer,
    credential: Arc<dyn CurseForgeCredentialSource>,
) -> CurseForgeApi {
    CurseForgeApi::with_base_url(
        credential,
        &HttpConfig::for_loopback_tests(),
        &format!("{}/v1", server.uri()),
    )
    .expect("a client builds")
}

fn configured(server: &MockServer) -> CurseForgeApi {
    api_against(
        server,
        Arc::new(ConfiguredApiKey::new(SecretString::new(FAKE_KEY))),
    )
}

#[tokio::test]
async fn an_authenticated_request_carries_the_api_key_header_and_decodes_the_documented_shape() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/games/432"))
        .and(header(API_KEY_HEADER, FAKE_KEY))
        .and(header("accept", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(MINECRAFT_GAME, "application/json"))
        .expect(1)
        .mount(&server)
        .await;

    let game = configured(&server)
        .game(MINECRAFT_GAME_ID, &CancellationToken::new())
        .await
        .expect("a documented response");

    assert_eq!(game.id, MINECRAFT_GAME_ID);
    assert_eq!(game.name, "Minecraft");
    assert_eq!(game.slug, "minecraft");
}

#[tokio::test]
async fn a_project_lookup_reads_the_numeric_identity_the_rest_of_the_api_uses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/mods/238222"))
        .and(header(API_KEY_HEADER, FAKE_KEY))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"data":{"id":238222,"gameId":432,"name":"Just Enough Items",
                "slug":"jei","summary":"View items and recipes","classId":6}}"#,
            "application/json",
        ))
        .mount(&server)
        .await;

    let project = configured(&server)
        .project(238_222, &CancellationToken::new())
        .await
        .expect("a documented response");
    assert_eq!(project.id, 238_222);
    assert_eq!(project.game_id, MINECRAFT_GAME_ID);
    assert_eq!(project.slug, "jei");
}

#[tokio::test]
async fn an_alternate_loopback_root_is_honored_rather_than_the_production_one() {
    // Two servers, so a client that ignored its configured root would answer
    // from the wrong one instead of merely failing.
    let wrong = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&wrong)
        .await;

    let right = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/games/432"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(MINECRAFT_GAME, "application/json"))
        .expect(1)
        .mount(&right)
        .await;

    let api = configured(&right);
    assert!(api.base_url().as_str().starts_with(&right.uri()));
    api.game(MINECRAFT_GAME_ID, &CancellationToken::new())
        .await
        .expect("the configured root answered");
}

#[tokio::test]
async fn a_request_with_no_configured_key_never_reaches_the_provider() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(MINECRAFT_GAME, "application/json"))
        .expect(0)
        .mount(&server)
        .await;

    let error = api_against(&server, Arc::new(ConfiguredApiKey::absent()))
        .game(MINECRAFT_GAME_ID, &CancellationToken::new())
        .await
        .expect_err("no key is configured");

    assert!(
        matches!(error, ModPlatformError::CredentialMissing { .. }),
        "{error:?}"
    );
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty(),
        "an unauthenticated request was made anyway"
    );
}

#[tokio::test]
async fn a_rejected_key_is_reported_as_a_credential_problem_rather_than_an_outage() {
    for status in [401, 403] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_raw(r#"{"error":"unauthorized"}"#, "application/json"),
            )
            .mount(&server)
            .await;

        let error = configured(&server)
            .game(MINECRAFT_GAME_ID, &CancellationToken::new())
            .await
            .expect_err("the key was refused");

        match &error {
            ModPlatformError::CredentialRejected {
                status: reported, ..
            } => assert_eq!(*reported, status),
            other => panic!("{status} produced {other:?}"),
        }
        assert!(error.is_credential_problem());
        assert!(!error.is_offline(), "a refused key is not an outage");
        assert!(!format!("{error:?}").contains(FAKE_KEY));
        assert!(!error.to_string().contains(FAKE_KEY));
    }
}

#[tokio::test]
async fn a_server_failure_is_reported_by_status_without_blaming_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let error = configured(&server)
        .game(MINECRAFT_GAME_ID, &CancellationToken::new())
        .await
        .expect_err("the provider failed");
    assert!(matches!(
        error,
        ModPlatformError::HttpStatus { status: 503, .. }
    ));
    assert!(!error.is_credential_problem());
}

#[tokio::test]
async fn a_body_that_is_not_the_documented_shape_is_refused_rather_than_guessed_at() {
    for body in [
        "not json at all",
        // Well-formed JSON, wrong document: no envelope.
        r#"{"id":432,"name":"Minecraft"}"#,
        // The envelope, with a payload that is the wrong type.
        r#"{"data":[]}"#,
        // Truncated part-way through.
        r#"{"data":{"id":432,"name":"Mine"#,
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;

        let error = configured(&server)
            .game(MINECRAFT_GAME_ID, &CancellationToken::new())
            .await
            .expect_err("the response is unreadable");
        assert!(
            matches!(error, ModPlatformError::ResponseInvalid { .. }),
            "{body}: {error:?}"
        );
    }
}

#[tokio::test]
async fn a_redirect_to_another_origin_does_not_receive_the_api_key() {
    // The failure this exists to prevent: an endpoint answers `302` pointing at
    // a host of its choosing, the client follows it, and the credential is
    // handed to whoever asked.
    let elsewhere = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(MINECRAFT_GAME, "application/json"))
        .mount(&elsewhere)
        .await;

    let api_host = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/games/432"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "location",
            format!("{}/v1/games/432", elsewhere.uri()).as_str(),
        ))
        .mount(&api_host)
        .await;

    let error = configured(&api_host)
        .game(MINECRAFT_GAME_ID, &CancellationToken::new())
        .await
        .expect_err("a redirect off the api origin is not followed");
    assert!(
        matches!(error, ModPlatformError::HttpStatus { status: 302, .. }),
        "{error:?}"
    );

    let leaked = elsewhere.received_requests().await.expect("recorded");
    assert!(
        leaked.is_empty(),
        "the redirect target was contacted at all: {leaked:?}"
    );
    assert!(
        !received_any_key(&api_host.received_requests().await.expect("recorded")).is_empty(),
        "the api origin itself must still be authenticated"
    );
}

#[tokio::test]
async fn a_redirect_that_stays_on_the_api_origin_is_followed_and_stays_authenticated() {
    // The other half of the invariant: scoping the credential to one origin
    // must not break an API that moves a path.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/games/432"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", "/v1/games/minecraft"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/games/minecraft"))
        .and(header(API_KEY_HEADER, FAKE_KEY))
        .respond_with(ResponseTemplate::new(200).set_body_raw(MINECRAFT_GAME, "application/json"))
        .expect(1)
        .mount(&server)
        .await;

    configured(&server)
        .game(MINECRAFT_GAME_ID, &CancellationToken::new())
        .await
        .expect("a same-origin redirect is followed");
}

// -- the exact-file endpoint ----------------------------------------------

/// A documented exact-file document, as `GET /v1/mods/{modId}/files/{fileId}`
/// answers for a file anyone may download.
const EXACT_FILE: &str = r#"{"data":{"id":4593548,"gameId":432,"modId":238222,"isAvailable":true,
    "displayName":"JEI 15.2.0.27","fileName":"jei-1.20.1-15.2.0.27.jar","releaseType":1,
    "fileStatus":4,"fileLength":1234567,
    "downloadUrl":"https://mediafilez.forgecdn.net/files/4593/548/jei.jar",
    "hashes":[{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1},
              {"value":"0123456789abcdef0123456789abcdef","algo":2}]}}"#;

/// The hosts a published download address may name, as production configures
/// them.
fn delivery_hosts() -> DomainAllowlist {
    DomainAllowlist::curseforge_artifact_defaults()
}

fn project() -> CurseForgeProjectId {
    CurseForgeProjectId::new(238_222).expect("a real id")
}

fn file_id() -> CurseForgeFileId {
    CurseForgeFileId::new(4_593_548).expect("a real id")
}

#[tokio::test]
async fn an_exact_file_is_requested_by_project_and_file_and_never_by_listing_versions() {
    // One authenticated GET at the exact endpoint. A version list, filtered
    // afterwards, is what lets a newer file be chosen; nothing here can.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/mods/238222/files/4593548"))
        .and(header(API_KEY_HEADER, FAKE_KEY))
        .respond_with(ResponseTemplate::new(200).set_body_raw(EXACT_FILE, "application/json"))
        .expect(1)
        .mount(&server)
        .await;

    let api = configured(&server);
    let release = api
        .begin_operation()
        .expect("a configured key")
        .file(
            project(),
            file_id(),
            &delivery_hosts(),
            &CancellationToken::new(),
        )
        .await
        .expect("the exact file");

    assert_eq!(release.project, project());
    assert_eq!(release.file, file_id());
    assert_eq!(release.artifact.file_name, "jei-1.20.1-15.2.0.27.jar");
    assert_eq!(release.artifact.size, Some(1_234_567));
    assert_eq!(
        release
            .artifact
            .hashes
            .iter()
            .map(|hash| hash.algorithm)
            .collect::<Vec<_>>(),
        vec![HashAlgorithm::Sha1],
        "only the digest this build can verify is kept"
    );

    // Nothing about the request or the answer names a version list.
    let requested: Vec<String> = server
        .received_requests()
        .await
        .expect("recorded")
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    assert_eq!(requested, vec!["/v1/mods/238222/files/4593548"]);
}

/// An exact-file response differing from [`EXACT_FILE`] only in `downloadUrl`.
fn exact_file_publishing(download_url: &str) -> String {
    EXACT_FILE.replace(
        r#""downloadUrl":"https://mediafilez.forgecdn.net/files/4593/548/jei.jar""#,
        download_url,
    )
}

async fn exact_file_answering(
    server: &MockServer,
    body: String,
) -> Result<ephervex_mod_platform::CurseForgeFileRelease, ModPlatformError> {
    Mock::given(method("GET"))
        .and(path("/v1/mods/238222/files/4593548"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .mount(server)
        .await;
    configured(server)
        .begin_operation()
        .expect("a configured key")
        .file(
            project(),
            file_id(),
            &delivery_hosts(),
            &CancellationToken::new(),
        )
        .await
}

#[tokio::test]
async fn an_exact_file_published_at_an_unapproved_host_is_refused_not_returned_as_a_download() {
    let server = MockServer::start().await;
    let error = exact_file_answering(
        &server,
        exact_file_publishing(r#""downloadUrl":"https://unapproved.invalid/a.jar""#),
    )
    .await
    .expect_err("must refuse");
    assert!(
        matches!(
            error,
            ModPlatformError::DownloadAddressRejected {
                provider: ModProvider::CurseForge,
                ..
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_exact_file_published_at_a_plain_http_address_is_refused() {
    let server = MockServer::start().await;
    let error = exact_file_answering(
        &server,
        exact_file_publishing(r#""downloadUrl":"http://mediafilez.forgecdn.net/files/jei.jar""#),
    )
    .await
    .expect_err("must refuse");
    assert!(
        matches!(error, ModPlatformError::DownloadAddressRejected { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_exact_file_without_a_usable_download_address_is_restricted_not_refused() {
    for (label, replacement) in [
        ("null", r#""downloadUrl":null"#),
        ("blank", r#""downloadUrl":"   ""#),
        ("absent", r#""unrelated":0"#),
    ] {
        let server = MockServer::start().await;
        let release = exact_file_answering(&server, exact_file_publishing(replacement))
            .await
            .unwrap_or_else(|error| panic!("{label}: {error:?}"));
        assert!(release.artifact.distribution.is_restricted(), "{label}");
    }
}

#[tokio::test]
async fn an_exact_file_at_an_approved_host_keeps_its_download_address() {
    let server = MockServer::start().await;
    let release = exact_file_answering(&server, EXACT_FILE.to_owned())
        .await
        .expect("the exact file");
    assert_eq!(
        release.artifact.distribution.download_url(),
        Some("https://mediafilez.forgecdn.net/files/4593/548/jei.jar")
    );
}

#[tokio::test]
async fn a_file_the_project_no_longer_has_is_reported_as_missing_rather_than_as_an_outage() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/mods/238222/files/4593548"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let api = configured(&server);
    let error = api
        .begin_operation()
        .expect("a configured key")
        .file(
            project(),
            file_id(),
            &delivery_hosts(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("must refuse");
    assert!(
        matches!(
            error,
            ModPlatformError::FileUnknown { ref project_id, ref file_id, .. }
                if project_id == "238222" && file_id == "4593548"
        ),
        "{error:?}"
    );
    assert!(!error.is_offline());
}

#[tokio::test]
async fn a_rejected_key_stays_a_credential_problem_and_never_quotes_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/mods/238222/files/4593548"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            format!(r#"{{"error":"bad key {FAKE_KEY}"}}"#),
            "application/json",
        ))
        .mount(&server)
        .await;

    let api = configured(&server);
    let error = api
        .begin_operation()
        .expect("a configured key")
        .file(
            project(),
            file_id(),
            &delivery_hosts(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("must refuse");
    assert!(error.is_credential_problem(), "{error:?}");
    assert!(matches!(
        error,
        ModPlatformError::CredentialRejected { status: 401, .. }
    ));
    assert!(!format!("{error:?}").contains(FAKE_KEY), "{error:?}");
    assert!(!error.to_string().contains(FAKE_KEY), "{error}");
}

#[tokio::test]
async fn an_exact_file_response_is_never_written_anywhere() {
    // The published third-party API terms forbid saving what the API returns.
    // The client has no cache to write to; this asserts it did not acquire one
    // by asserting nothing appeared in a directory handed to it as a working
    // root.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/mods/238222/files/4593548"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(EXACT_FILE, "application/json"))
        .mount(&server)
        .await;

    let root = tempfile::tempdir().expect("temp dir");
    let before = std::fs::read_dir(root.path()).expect("read").count();

    let api = configured(&server);
    api.begin_operation()
        .expect("a configured key")
        .file(
            project(),
            file_id(),
            &delivery_hosts(),
            &CancellationToken::new(),
        )
        .await
        .expect("the exact file");

    assert_eq!(
        std::fs::read_dir(root.path()).expect("read").count(),
        before,
        "a CurseForge response was persisted"
    );
}

/// The requests that carried the credential header.
fn received_any_key(requests: &[Request]) -> Vec<&Request> {
    requests
        .iter()
        .filter(|request| request.headers.contains_key(API_KEY_HEADER))
        .collect()
}

/// The fingerprint endpoint is what identifies a jar that arrived with no id
/// of its own, which is every file in a published server archive.
#[tokio::test]
async fn a_fingerprint_lookup_identifies_a_file_by_its_bytes() {
    let digest = "a".repeat(40);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/fingerprints"))
        .and(header(API_KEY_HEADER, FAKE_KEY))
        .and(body_json(serde_json::json!({ "fingerprints": [2_038_518_775_u32] })))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"data":{{"isCacheBuilt":true,"exactMatches":[{{"id":238222,"file":{{"id":4712354,"modId":238222,"isAvailable":true,"fileName":"jei.jar","hashes":[{{"value":"{digest}","algo":1}}]}}}}],"exactFingerprints":[2038518775]}}}}"#
        )))
        .mount(&server)
        .await;

    let api = configured(&server);
    let operation = api.begin_operation().expect("an operation begins");
    let wanted = vec![(
        2_038_518_775_u32,
        Hash::new(HashAlgorithm::Sha1, &digest).expect("sha1"),
    )];

    let recognized = operation
        .fingerprints(&wanted, &CancellationToken::new())
        .await
        .expect("the lookup answers");

    let found = recognized.get(&digest).expect("the file is recognized");
    assert_eq!(found.provider, ModProvider::CurseForge);
    assert_eq!(found.project_id, "238222");
    assert_eq!(found.file_id.as_deref(), Some("4712354"));
}

/// The file document that identified a jar also says what that exact file
/// declares, and a file declaring nothing is an answer, not a missing one.
#[tokio::test]
async fn a_fingerprint_match_carries_what_its_file_declares() {
    let feature = "a".repeat(40);
    let library = "b".repeat(40);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/fingerprints"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"data":{{"exactMatches":[{{"id":238222,"file":{{"id":4712354,"modId":238222,"isAvailable":true,"fileName":"feature.jar","hashes":[{{"value":"{feature}","algo":1}}],"dependencies":[{{"modId":306612,"relationType":3}},{{"modId":32274,"relationType":2}}]}}}},{{"id":306612,"file":{{"id":4593548,"modId":306612,"isAvailable":true,"fileName":"library.jar","hashes":[{{"value":"{library}","algo":1}}],"dependencies":[]}}}}]}}}}"#
        )))
        .mount(&server)
        .await;

    let api = configured(&server);
    let operation = api.begin_operation().expect("an operation begins");
    let wanted = vec![
        (
            1_u32,
            Hash::new(HashAlgorithm::Sha1, &feature).expect("sha1"),
        ),
        (
            2_u32,
            Hash::new(HashAlgorithm::Sha1, &library).expect("sha1"),
        ),
    ];

    let recognized = operation
        .fingerprints(&wanted, &CancellationToken::new())
        .await
        .expect("the lookup answers");

    let declared = |kind, project: &str| ModDependency {
        kind,
        project_id: Some(project.to_owned()),
        version_id: None,
        file_name: None,
    };
    assert_eq!(
        recognized.get(&feature).expect("recognized").dependencies,
        [
            declared(ModRelationKind::Required, "306612"),
            declared(ModRelationKind::Optional, "32274"),
        ]
    );
    assert_eq!(
        recognized.get(&library).expect("recognized").dependencies,
        []
    );
}

/// Which side a file runs on is the file's own claim, carried with the match.
#[tokio::test]
async fn a_fingerprint_match_carries_the_sides_its_file_declares() {
    let client_only = "a".repeat(40);
    let both = "b".repeat(40);
    let silent = "c".repeat(40);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/fingerprints"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"data":{{"exactMatches":[{{"id":1,"file":{{"id":11,"modId":1,"isAvailable":true,"fileName":"client.jar","gameVersions":["1.20.1","NeoForge","Client"],"hashes":[{{"value":"{client_only}","algo":1}}],"dependencies":[]}}}},{{"id":2,"file":{{"id":22,"modId":2,"isAvailable":true,"fileName":"both.jar","gameVersions":["1.20.1","Client","Server"],"hashes":[{{"value":"{both}","algo":1}}],"dependencies":[]}}}},{{"id":3,"file":{{"id":33,"modId":3,"isAvailable":true,"fileName":"silent.jar","gameVersions":["1.20.1","NeoForge"],"hashes":[{{"value":"{silent}","algo":1}}],"dependencies":[]}}}}]}}}}"#
        )))
        .mount(&server)
        .await;

    let api = configured(&server);
    let operation = api.begin_operation().expect("an operation begins");
    let wanted: Vec<_> = [(1_u32, &client_only), (2, &both), (3, &silent)]
        .into_iter()
        .map(|(fingerprint, digest)| {
            (
                fingerprint,
                Hash::new(HashAlgorithm::Sha1, digest).expect("sha1"),
            )
        })
        .collect();

    let recognized = operation
        .fingerprints(&wanted, &CancellationToken::new())
        .await
        .expect("the lookup answers");

    let sides = |digest: &str| {
        let found = recognized.get(digest).expect("recognized");
        (found.client_side, found.server_side)
    };
    assert_eq!(
        sides(&client_only),
        (SideSupport::Required, SideSupport::Unsupported)
    );
    assert_eq!(sides(&both), (SideSupport::Required, SideSupport::Required));
    assert_eq!(
        sides(&silent),
        (SideSupport::Unknown, SideSupport::Unknown),
        "a file naming neither side declares nothing"
    );
}

/// A file whose relation names no project cannot be described honestly, so it
/// stays unidentified; it must not be recognized as declaring nothing, and it
/// must not stop the rest of the batch from being recognized.
#[tokio::test]
async fn a_match_with_an_unusable_relation_is_left_unrecognized_without_failing_the_batch() {
    let broken = "a".repeat(40);
    let sound = "b".repeat(40);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/fingerprints"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"data":{{"exactMatches":[{{"id":238222,"file":{{"id":4712354,"modId":238222,"isAvailable":true,"fileName":"broken.jar","hashes":[{{"value":"{broken}","algo":1}}],"dependencies":[{{"modId":0,"relationType":3}}]}}}},{{"id":306612,"file":{{"id":4593548,"modId":306612,"isAvailable":true,"fileName":"sound.jar","hashes":[{{"value":"{sound}","algo":1}}],"dependencies":[]}}}}]}}}}"#
        )))
        .mount(&server)
        .await;

    let api = configured(&server);
    let operation = api.begin_operation().expect("an operation begins");
    let wanted = vec![
        (
            1_u32,
            Hash::new(HashAlgorithm::Sha1, &broken).expect("sha1"),
        ),
        (2_u32, Hash::new(HashAlgorithm::Sha1, &sound).expect("sha1")),
    ];

    let recognized = operation
        .fingerprints(&wanted, &CancellationToken::new())
        .await
        .expect("the lookup answers");

    assert!(!recognized.contains_key(&broken));
    assert!(recognized.contains_key(&sound));
}

/// A fingerprint is a lookup key, not a digest. An answer whose own SHA-1 is
/// not the file's is a collision or the wrong file, and identifying the jar
/// from it would put a made-up release in the server's recipe.
#[tokio::test]
async fn a_match_whose_digest_disagrees_is_not_believed() {
    let asked = "a".repeat(40);
    let answered = "b".repeat(40);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/fingerprints"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            r#"{{"data":{{"exactMatches":[{{"id":238222,"file":{{"id":4712354,"modId":238222,"isAvailable":true,"fileName":"other.jar","hashes":[{{"value":"{answered}","algo":1}}]}}}}]}}}}"#
        )))
        .mount(&server)
        .await;

    let api = configured(&server);
    let operation = api.begin_operation().expect("an operation begins");
    let wanted = vec![(1_u32, Hash::new(HashAlgorithm::Sha1, &asked).expect("sha1"))];

    let recognized = operation
        .fingerprints(&wanted, &CancellationToken::new())
        .await
        .expect("the lookup answers");

    assert!(
        recognized.is_empty(),
        "an answer about other bytes identifies nothing"
    );
}

/// Nothing to ask about is not a request.
#[tokio::test]
async fn an_empty_fingerprint_batch_contacts_nobody() {
    let server = MockServer::start().await;
    let api = configured(&server);
    let operation = api.begin_operation().expect("an operation begins");

    let recognized = operation
        .fingerprints(&[], &CancellationToken::new())
        .await
        .expect("no request is made");

    assert!(recognized.is_empty());
    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );
}

// -- batched exact files and projects -------------------------------------

const BATCH_FILES_PATH: &str = "/v1/mods/files";
const BATCH_PROJECTS_PATH: &str = "/v1/mods";

fn project_of(id: u32) -> CurseForgeProjectId {
    CurseForgeProjectId::new(id).expect("a real id")
}

fn file_of(id: u32) -> CurseForgeFileId {
    CurseForgeFileId::new(id).expect("a real id")
}

/// A file document for `file` of `project` that anyone may download.
fn batch_file_document(project: u32, file: u32) -> String {
    batch_file_document_with(
        project,
        file,
        true,
        4,
        Some("https://mediafilez.forgecdn.net/f.jar"),
    )
}

fn batch_file_document_with(
    project: u32,
    file: u32,
    available: bool,
    status: u32,
    download_url: Option<&str>,
) -> String {
    let url = download_url.map_or_else(|| "null".to_owned(), |url| format!("\"{url}\""));
    format!(
        r#"{{"id":{file},"gameId":432,"modId":{project},"isAvailable":{available},
        "displayName":"f{file}","fileName":"f{file}.jar","releaseType":1,
        "fileStatus":{status},"fileLength":10,"downloadUrl":{url},
        "hashes":[{{"value":"da39a3ee5e6b4b0d3255bfef95601890afd80709","algo":1}}]}}"#
    )
}

fn batch_project_document(id: u32) -> String {
    format!(
        r#"{{"id":{id},"gameId":432,"name":"Project {id}","slug":"project-{id}","summary":"s","classId":6}}"#
    )
}

fn data_envelope(documents: &[String]) -> String {
    format!(r#"{{"data":[{}]}}"#, documents.join(","))
}

async fn mount_batch(server: &MockServer, route: &str, status: u16, body: String) {
    Mock::given(method("POST"))
        .and(path(route))
        .and(header(API_KEY_HEADER, FAKE_KEY))
        .respond_with(ResponseTemplate::new(status).set_body_raw(body, "application/json"))
        .mount(server)
        .await;
}

/// The id lists each POST to `route` carried, under `key`, in request order.
async fn posted_ids(server: &MockServer, route: &str, key: &str) -> Vec<Vec<u64>> {
    server
        .received_requests()
        .await
        .expect("recorded")
        .iter()
        .filter(|request| request.url.path() == route)
        .map(|request| {
            assert_eq!(request.method.as_str(), "POST");
            let body: serde_json::Value = request.body_json().expect("a json body");
            body[key]
                .as_array()
                .unwrap_or_else(|| panic!("no `{key}` array in {body}"))
                .iter()
                .map(|id| id.as_u64().expect("a numeric id"))
                .collect()
        })
        .collect()
}

/// Answers every batch file request with the documents for the ids it carries.
struct EchoFiles;

impl wiremock::Respond for EchoFiles {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: serde_json::Value = request.body_json().expect("a json body");
        let documents: Vec<String> = body["fileIds"]
            .as_array()
            .expect("file ids")
            .iter()
            .map(|id| {
                let file = u32::try_from(id.as_u64().expect("numeric")).expect("fits");
                batch_file_document(1000 + file % 7, file)
            })
            .collect();
        ResponseTemplate::new(200).set_body_raw(data_envelope(&documents), "application/json")
    }
}

/// Answers every batch project request with the documents for the ids it carries.
struct EchoProjects;

impl wiremock::Respond for EchoProjects {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: serde_json::Value = request.body_json().expect("a json body");
        let documents: Vec<String> = body["modIds"]
            .as_array()
            .expect("project ids")
            .iter()
            .map(|id| {
                batch_project_document(u32::try_from(id.as_u64().expect("numeric")).expect("fits"))
            })
            .collect();
        ResponseTemplate::new(200).set_body_raw(data_envelope(&documents), "application/json")
    }
}

async fn exact_files_from(
    server: &MockServer,
    wanted: &[(CurseForgeProjectId, CurseForgeFileId)],
) -> Result<
    Vec<Result<ephervex_mod_platform::CurseForgeFileRelease, ModPlatformError>>,
    ModPlatformError,
> {
    configured(server)
        .begin_operation()
        .expect("a configured key")
        .exact_files(wanted, &delivery_hosts(), &CancellationToken::new())
        .await
}

async fn exact_projects_from(
    server: &MockServer,
    projects: &[CurseForgeProjectId],
) -> Result<Vec<Result<ephervex_mod_platform::CurseForgeProject, ModPlatformError>>, ModPlatformError>
{
    configured(server)
        .begin_operation()
        .expect("a configured key")
        .exact_projects(projects, &CancellationToken::new())
        .await
}

#[tokio::test]
async fn several_exact_files_are_resolved_in_one_post_carrying_exactly_the_wanted_file_ids() {
    let server = MockServer::start().await;
    mount_batch(
        &server,
        BATCH_FILES_PATH,
        200,
        data_envelope(&[
            batch_file_document(10, 101),
            batch_file_document(20, 202),
            batch_file_document(30, 303),
        ]),
    )
    .await;

    let outcomes = exact_files_from(
        &server,
        &[
            (project_of(10), file_of(101)),
            (project_of(20), file_of(202)),
            (project_of(30), file_of(303)),
        ],
    )
    .await
    .expect("the request succeeds");

    assert_eq!(outcomes.len(), 3);
    assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
    assert_eq!(
        posted_ids(&server, BATCH_FILES_PATH, "fileIds").await,
        vec![vec![101, 202, 303]]
    );
    assert_eq!(server.received_requests().await.expect("recorded").len(), 1);
}

#[tokio::test]
async fn a_file_id_wanted_under_two_projects_is_sent_once_and_only_the_matching_pair_resolves() {
    let server = MockServer::start().await;
    mount_batch(
        &server,
        BATCH_FILES_PATH,
        200,
        data_envelope(&[batch_file_document(10, 101)]),
    )
    .await;

    let outcomes = exact_files_from(
        &server,
        &[
            (project_of(10), file_of(101)),
            (project_of(20), file_of(101)),
        ],
    )
    .await
    .expect("the request succeeds");

    assert!(outcomes[0].is_ok(), "{outcomes:?}");
    assert!(
        matches!(outcomes[1], Err(ModPlatformError::FileUnknown { .. })),
        "{outcomes:?}"
    );
    assert_eq!(
        posted_ids(&server, BATCH_FILES_PATH, "fileIds").await,
        vec![vec![101]],
        "the id is sent once"
    );
}

#[tokio::test]
async fn exact_file_outcomes_follow_the_order_asked_whatever_order_the_server_answers_in() {
    let server = MockServer::start().await;
    mount_batch(
        &server,
        BATCH_FILES_PATH,
        200,
        data_envelope(&[
            batch_file_document(30, 303),
            batch_file_document(10, 101),
            batch_file_document(20, 202),
        ]),
    )
    .await;

    let outcomes = exact_files_from(
        &server,
        &[
            (project_of(10), file_of(101)),
            (project_of(20), file_of(202)),
            (project_of(30), file_of(303)),
        ],
    )
    .await
    .expect("the request succeeds");

    let resolved: Vec<(u32, u32)> = outcomes
        .into_iter()
        .map(|outcome| {
            let release = outcome.expect("resolved");
            (release.project.get(), release.file.get())
        })
        .collect();
    assert_eq!(resolved, vec![(10, 101), (20, 202), (30, 303)]);
}

#[tokio::test]
async fn an_exact_file_the_server_leaves_out_is_unknown_while_the_others_still_resolve() {
    let server = MockServer::start().await;
    mount_batch(
        &server,
        BATCH_FILES_PATH,
        200,
        data_envelope(&[batch_file_document(10, 101), batch_file_document(30, 303)]),
    )
    .await;

    let outcomes = exact_files_from(
        &server,
        &[
            (project_of(10), file_of(101)),
            (project_of(20), file_of(202)),
            (project_of(30), file_of(303)),
        ],
    )
    .await
    .expect("the request succeeds");

    assert_eq!(outcomes[0].as_ref().expect("resolved").file.get(), 101);
    assert!(
        matches!(
            &outcomes[1],
            Err(ModPlatformError::FileUnknown { project_id, file_id, .. })
                if project_id == "20" && file_id == "202"
        ),
        "{:?}",
        outcomes[1]
    );
    assert_eq!(outcomes[2].as_ref().expect("resolved").file.get(), 303);
}

#[tokio::test]
async fn an_exact_file_returned_only_under_another_project_is_unknown_and_never_used() {
    let server = MockServer::start().await;
    mount_batch(
        &server,
        BATCH_FILES_PATH,
        200,
        data_envelope(&[batch_file_document(99, 101), batch_file_document(30, 303)]),
    )
    .await;

    let outcomes = exact_files_from(
        &server,
        &[
            (project_of(10), file_of(101)),
            (project_of(30), file_of(303)),
        ],
    )
    .await
    .expect("the request succeeds");

    assert!(
        matches!(
            &outcomes[0],
            Err(ModPlatformError::FileUnknown { project_id, file_id, .. })
                if project_id == "10" && file_id == "101"
        ),
        "{:?}",
        outcomes[0]
    );
    assert!(outcomes[1].is_ok(), "{:?}", outcomes[1]);
}

#[tokio::test]
async fn more_than_a_hundred_exact_files_are_sent_as_several_requests_and_each_gets_an_outcome() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(BATCH_FILES_PATH))
        .respond_with(EchoFiles)
        .mount(&server)
        .await;

    let wanted: Vec<_> = (1..=250u32)
        .map(|file| (project_of(1000 + file % 7), file_of(file)))
        .collect();
    let outcomes = exact_files_from(&server, &wanted)
        .await
        .expect("every request succeeds");

    assert_eq!(outcomes.len(), 250);
    for (outcome, (project, file)) in outcomes.iter().zip(&wanted) {
        let release = outcome.as_ref().expect("resolved");
        assert_eq!((release.project, release.file), (*project, *file));
    }
    let batches = posted_ids(&server, BATCH_FILES_PATH, "fileIds").await;
    assert_eq!(batches.len(), 3);
    assert!(batches.iter().all(|ids| ids.len() <= 100), "{batches:?}");
    let mut sent: Vec<u64> = batches.into_iter().flatten().collect();
    sent.sort_unstable();
    assert_eq!(sent, (1..=250u64).collect::<Vec<_>>());
}

#[tokio::test]
async fn each_pair_in_an_exact_file_batch_is_judged_on_its_own_without_spoiling_the_others() {
    let server = MockServer::start().await;
    mount_batch(
        &server,
        BATCH_FILES_PATH,
        200,
        data_envelope(&[
            batch_file_document(10, 101),
            // Withdrawn, no address: nothing serves it.
            batch_file_document_with(20, 202, false, 4, None),
            // Archived but still served from an address: still the pinned file.
            batch_file_document_with(
                30,
                303,
                false,
                8,
                Some("https://mediafilez.forgecdn.net/archived.jar"),
            ),
            // An address outside the approved hosts.
            batch_file_document_with(40, 404, true, 4, Some("https://unapproved.invalid/a.jar")),
            batch_file_document(50, 505),
        ]),
    )
    .await;

    let outcomes = exact_files_from(
        &server,
        &[
            (project_of(10), file_of(101)),
            (project_of(20), file_of(202)),
            (project_of(30), file_of(303)),
            (project_of(40), file_of(404)),
            (project_of(50), file_of(505)),
        ],
    )
    .await
    .expect("the request succeeds");

    assert!(outcomes[0].is_ok(), "{:?}", outcomes[0]);
    assert!(
        matches!(
            &outcomes[1],
            Err(ModPlatformError::FileUnavailable { project_id, file_id, .. })
                if project_id == "20" && file_id == "202"
        ),
        "{:?}",
        outcomes[1]
    );
    let archived = outcomes[2].as_ref().expect("an archived file is accepted");
    assert_eq!(
        archived.artifact.distribution.download_url(),
        Some("https://mediafilez.forgecdn.net/archived.jar")
    );
    assert!(
        matches!(
            &outcomes[3],
            Err(ModPlatformError::DownloadAddressRejected { .. })
        ),
        "{:?}",
        outcomes[3]
    );
    assert!(outcomes[4].is_ok(), "{:?}", outcomes[4]);
}

#[tokio::test]
async fn a_failing_exact_file_batch_request_fails_the_whole_call() {
    for status in [403, 500] {
        let server = MockServer::start().await;
        mount_batch(&server, BATCH_FILES_PATH, status, "{}".to_owned()).await;

        let error = exact_files_from(
            &server,
            &[
                (project_of(10), file_of(101)),
                (project_of(20), file_of(202)),
            ],
        )
        .await
        .expect_err("the request failed");

        match (status, &error) {
            (403, ModPlatformError::CredentialRejected { status: 403, .. })
            | (500, ModPlatformError::HttpStatus { status: 500, .. }) => {}
            _ => panic!("{status} produced {error:?}"),
        }
    }
}

#[tokio::test]
async fn an_empty_exact_file_batch_sends_no_request() {
    let server = MockServer::start().await;

    let outcomes = exact_files_from(&server, &[])
        .await
        .expect("nothing to ask about");

    assert!(outcomes.is_empty());
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty()
    );
}

#[tokio::test]
async fn several_exact_projects_are_resolved_in_one_post_and_keep_the_order_asked() {
    let server = MockServer::start().await;
    mount_batch(
        &server,
        BATCH_PROJECTS_PATH,
        200,
        data_envelope(&[
            batch_project_document(30),
            batch_project_document(10),
            batch_project_document(20),
        ]),
    )
    .await;

    let outcomes = exact_projects_from(&server, &[project_of(10), project_of(20), project_of(30)])
        .await
        .expect("the request succeeds");

    let resolved: Vec<u32> = outcomes
        .into_iter()
        .map(|outcome| outcome.expect("resolved").id)
        .collect();
    assert_eq!(resolved, vec![10, 20, 30]);
    assert_eq!(
        posted_ids(&server, BATCH_PROJECTS_PATH, "modIds").await,
        vec![vec![10, 20, 30]]
    );
    assert_eq!(server.received_requests().await.expect("recorded").len(), 1);
}

#[tokio::test]
async fn an_exact_project_the_server_leaves_out_is_unknown_while_the_others_still_resolve() {
    let server = MockServer::start().await;
    mount_batch(
        &server,
        BATCH_PROJECTS_PATH,
        200,
        data_envelope(&[batch_project_document(10), batch_project_document(30)]),
    )
    .await;

    let outcomes = exact_projects_from(&server, &[project_of(10), project_of(20), project_of(30)])
        .await
        .expect("the request succeeds");

    assert_eq!(outcomes[0].as_ref().expect("resolved").id, 10);
    assert!(
        matches!(
            &outcomes[1],
            Err(ModPlatformError::ProjectUnknown { reference, .. }) if reference == "20"
        ),
        "{:?}",
        outcomes[1]
    );
    assert_eq!(outcomes[2].as_ref().expect("resolved").id, 30);
}

#[tokio::test]
async fn more_than_a_hundred_exact_projects_are_sent_as_several_requests_and_each_gets_an_outcome()
{
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(BATCH_PROJECTS_PATH))
        .respond_with(EchoProjects)
        .mount(&server)
        .await;

    let projects: Vec<_> = (1..=250u32).map(project_of).collect();
    let outcomes = exact_projects_from(&server, &projects)
        .await
        .expect("every request succeeds");

    assert_eq!(outcomes.len(), 250);
    for (outcome, project) in outcomes.iter().zip(&projects) {
        assert_eq!(outcome.as_ref().expect("resolved").id, project.get());
    }
    let batches = posted_ids(&server, BATCH_PROJECTS_PATH, "modIds").await;
    assert_eq!(batches.len(), 3);
    assert!(batches.iter().all(|ids| ids.len() <= 100), "{batches:?}");
    let mut sent: Vec<u64> = batches.into_iter().flatten().collect();
    sent.sort_unstable();
    assert_eq!(sent, (1..=250u64).collect::<Vec<_>>());
}

#[tokio::test]
async fn a_failing_exact_project_batch_request_fails_the_whole_call() {
    for status in [403, 500] {
        let server = MockServer::start().await;
        mount_batch(&server, BATCH_PROJECTS_PATH, status, "{}".to_owned()).await;

        let error = exact_projects_from(&server, &[project_of(10), project_of(20)])
            .await
            .expect_err("the request failed");

        match (status, &error) {
            (403, ModPlatformError::CredentialRejected { status: 403, .. })
            | (500, ModPlatformError::HttpStatus { status: 500, .. }) => {}
            _ => panic!("{status} produced {error:?}"),
        }
    }
}

#[tokio::test]
async fn an_empty_exact_project_batch_sends_no_request() {
    let server = MockServer::start().await;

    let outcomes = exact_projects_from(&server, &[])
        .await
        .expect("nothing to ask about");

    assert!(outcomes.is_empty());
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty()
    );
}
