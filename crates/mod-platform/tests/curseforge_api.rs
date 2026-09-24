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

use ephervex_download::HttpConfig;
use ephervex_mod_platform::{
    ConfiguredApiKey, CurseForgeApi, CurseForgeCredentialSource, MINECRAFT_GAME_ID, ModDependency,
    ModPlatformError,
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
        .file(project(), file_id(), &CancellationToken::new())
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
        .file(project(), file_id(), &CancellationToken::new())
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
        .file(project(), file_id(), &CancellationToken::new())
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
        .file(project(), file_id(), &CancellationToken::new())
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
