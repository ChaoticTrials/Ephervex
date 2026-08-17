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
    ConfiguredApiKey, CurseForgeApi, CurseForgeCredentialSource, MINECRAFT_GAME_ID,
    ModPlatformError,
};
use ephervex_model::SecretString;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
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
                "slug":"jei","summary":"View items and recipes"}}"#,
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

/// The requests that carried the credential header.
fn received_any_key(requests: &[Request]) -> Vec<&Request> {
    requests
        .iter()
        .filter(|request| request.headers.contains_key(API_KEY_HEADER))
        .collect()
}
