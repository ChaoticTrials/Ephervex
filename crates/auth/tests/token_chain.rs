//! The whole authentication chain against a loopback HTTP server
//!
//! Nothing here touches Microsoft, Xbox Live, or Minecraft Services. CI that
//! depends on a third party's uptime fails for reasons that have nothing to do
//! with the change under test - and CI that depends on a *real Microsoft
//! account* cannot exist at all, because there is no way to hold one without
//! committing a credential (ADR 0004).
//!
//! So all five services are served by one `wiremock` instance from the
//! fixtures in `fixtures/microsoft/`, which are documented response shapes with
//! every token replaced by an obvious placeholder. That substitution is honest
//! rather than a shortcut: this crate never validates a signature, decodes a
//! JWT, or inspects a token beyond handing it to the next leg, so a real
//! capture would exercise exactly the same code.
//!
//! # What these tests are standing in for
//!
//! account and authentication support's regression coverage are written against a real owned account, and
//! an automated suite cannot have one. What it *can* do is pin every decision
//! the chain makes given a response - which leg ran, in which order, with which
//! body, and what each failure turns into. The residue that genuinely needs a
//! human is listed in `docs/architecture.md`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use ephervex_auth::{
    AccountProvider, AuthError, AuthEvent, AuthObserver, AuthStage, CredentialChangeOrder,
    CredentialKey, CredentialRemoval, CredentialStorage, CredentialStore, Endpoints,
    EntitlementLookup, LoginFlow, LoginRequest, MemoryStore, MicrosoftAccountProvider,
    ProcessChangeOrder, SecretString, XstsDenial,
};
use ephervex_download::HttpConfig;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CLIENT_ID: &str = "767ea465-0bba-443b-a06b-831c71777b90";
const ACCOUNT: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";

const TOKEN: &str = include_str!("../../../fixtures/microsoft/microsoft-token.json");
const TOKEN_ROTATED: &str =
    include_str!("../../../fixtures/microsoft/microsoft-token-rotated.json");
const DEVICE_CODE: &str = include_str!("../../../fixtures/microsoft/microsoft-device-code.json");
const PENDING: &str =
    include_str!("../../../fixtures/microsoft/microsoft-authorization-pending.json");
const DECLINED: &str =
    include_str!("../../../fixtures/microsoft/microsoft-authorization-declined.json");
const EXPIRED: &str = include_str!("../../../fixtures/microsoft/microsoft-expired-token.json");
const INVALID_GRANT: &str =
    include_str!("../../../fixtures/microsoft/microsoft-invalid-grant.json");
const XBOX: &str = include_str!("../../../fixtures/microsoft/xbox-authenticate.json");
const XSTS: &str = include_str!("../../../fixtures/microsoft/xsts-authorize.json");
const XSTS_DENIED: &str = include_str!("../../../fixtures/microsoft/xsts-denied.json");
const MC_LOGIN: &str = include_str!("../../../fixtures/microsoft/minecraft-login.json");
const ENTITLEMENTS: &str = include_str!("../../../fixtures/microsoft/minecraft-entitlements.json");
const ENTITLEMENTS_GAME_PASS: &str =
    include_str!("../../../fixtures/microsoft/minecraft-entitlements-game-pass.json");
const ENTITLEMENTS_NONE: &str =
    include_str!("../../../fixtures/microsoft/minecraft-entitlements-none.json");
const PROFILE: &str = include_str!("../../../fixtures/microsoft/minecraft-profile.json");
const PROFILE_MISSING: &str =
    include_str!("../../../fixtures/microsoft/minecraft-profile-not-found.json");

// ------------------------------------------------------------------ scaffolding

/// Everything after the Microsoft token, mounted and succeeding.
async fn mount_chain(server: &MockServer) {
    mount_xbox(server, 200, XBOX).await;
    mount_xsts(server, 200, XSTS).await;
    mount_minecraft_login(server, 200, MC_LOGIN).await;
    mount_entitlements(server, 200, ENTITLEMENTS).await;
    mount_profile(server, 200, PROFILE).await;
}

async fn mount_xbox(server: &MockServer, status: u16, body: &str) {
    Mock::given(method("POST"))
        .and(path("/user/authenticate"))
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .mount(server)
        .await;
}

async fn mount_xsts(server: &MockServer, status: u16, body: &str) {
    Mock::given(method("POST"))
        .and(path("/xsts/authorize"))
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .mount(server)
        .await;
}

async fn mount_minecraft_login(server: &MockServer, status: u16, body: &str) {
    Mock::given(method("POST"))
        .and(path("/authentication/login_with_xbox"))
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .mount(server)
        .await;
}

async fn mount_entitlements(server: &MockServer, status: u16, body: &str) {
    Mock::given(method("GET"))
        .and(path("/entitlements/mcstore"))
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .mount(server)
        .await;
}

async fn mount_profile(server: &MockServer, status: u16, body: &str) {
    Mock::given(method("GET"))
        .and(path("/minecraft/profile"))
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .mount(server)
        .await;
}

async fn mount_token(server: &MockServer, status: u16, body: &str) {
    Mock::given(method("POST"))
        .and(path("/consumers/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .mount(server)
        .await;
}

async fn mount_device_code(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/consumers/oauth2/v2.0/devicecode"))
        .respond_with(ResponseTemplate::new(200).set_body_string(DEVICE_CODE))
        .mount(server)
        .await;
}

async fn mount_device_code_with_lifetime(server: &MockServer, expires_in: Option<u64>) {
    let mut body: serde_json::Value = serde_json::from_str(DEVICE_CODE).expect("fixture parses");
    match expires_in {
        Some(seconds) => body["expires_in"] = seconds.into(),
        None => {
            body.as_object_mut().expect("object").remove("expires_in");
        }
    }
    Mock::given(method("POST"))
        .and(path("/consumers/oauth2/v2.0/devicecode"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body.to_string()))
        .mount(server)
        .await;
}

fn provider(server: &MockServer, store: Arc<dyn CredentialStore>) -> MicrosoftAccountProvider {
    MicrosoftAccountProvider::with_endpoints(
        CLIENT_ID,
        store,
        Endpoints::with_base(&server.uri()),
        &HttpConfig::for_loopback_tests(),
    )
    .expect("provider")
}

fn account() -> Uuid {
    Uuid::parse_str(ACCOUNT).expect("uuid")
}

/// Drives the browser flow by playing the part of the browser: takes the URL
/// off the event channel, extracts the `redirect_uri` and `state` Ephervex
/// generated, and performs the redirect Microsoft would have performed.
async fn drive_browser_login(
    provider: &MicrosoftAccountProvider,
    outcome: impl Fn(&str, &str) -> String + Send + 'static,
) -> Result<ephervex_auth::Session, AuthError> {
    let (observer, mut events) = AuthObserver::channel();

    let browser = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Some(AuthEvent::OpenBrowser { url }) => {
                    let parsed = reqwest::Url::parse(&url).expect("an authorization url");
                    let query: std::collections::HashMap<_, _> = parsed
                        .query_pairs()
                        .map(|(k, v)| (k.into_owned(), v.into_owned()))
                        .collect();
                    let redirect = query["redirect_uri"].clone();
                    let state = query["state"].clone();
                    let target = outcome(&redirect, &state);
                    // A plain client, because this is standing in for a browser
                    // and must not inherit any of Ephervex's policies.
                    let _ = reqwest::Client::new().get(&target).send().await;
                    return query;
                }
                Some(_) => {}
                None => panic!("the login never asked for a browser"),
            }
        }
    });

    let session = provider
        .login(
            &LoginRequest {
                flow: LoginFlow::Browser,
                timeout: Duration::from_secs(10),
            },
            &observer,
            &CancellationToken::new(),
        )
        .await;
    browser.await.expect("the browser stand-in finished");
    let login = session?;
    keep(provider, &login);
    Ok(login.session)
}

/// Stores a login's credential the way the account service does, under the
/// credential-change order.
fn keep(provider: &MicrosoftAccountProvider, login: &ephervex_auth::Login) {
    let held = ProcessChangeOrder.hold().expect("order");
    provider.keep_login(login, &held).expect("keep");
}

// ------------------------------------------------------------------ the flows

#[tokio::test]
async fn the_browser_flow_completes_the_whole_chain_and_stores_the_refresh_token() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_chain(&server).await;

    let store = Arc::new(MemoryStore::new());
    let provider = provider(&server, store.clone());

    let session = drive_browser_login(&provider, |redirect, state| {
        format!("{redirect}/?code=THE-AUTH-CODE&state={state}")
    })
    .await
    .expect("the chain completes");

    assert_eq!(session.account.id, account());
    assert_eq!(session.account.name, "TestAccount");
    assert!(
        session.is_authenticated(),
        "a completed chain is authenticated"
    );
    assert_eq!(
        session.access_token().expect("a token").expose(),
        "FAKE-MINECRAFT-ACCESS-TOKEN"
    );
    assert!(
        session
            .entitlements()
            .expect("a microsoft session reports entitlements")
            .owns_game()
    );
    assert!(session.is_usable(chrono::TimeDelta::hours(1)));

    // The refresh token - and only the refresh token - was persisted.
    let stored = store
        .load(&CredentialKey::MicrosoftRefreshToken(account()))
        .expect("load")
        .expect("a credential was stored");
    assert_eq!(stored.expose(), "FAKE-MICROSOFT-REFRESH-TOKEN");
    assert!(
        store
            .load(&CredentialKey::CurseForgeApiKey)
            .expect("load")
            .is_none()
    );
}

#[tokio::test]
async fn the_browser_flow_sends_pkce_and_redeems_with_the_matching_verifier() {
    let server = MockServer::start().await;
    // Matching on the verifier is the point: a redemption without it, or with
    // the wrong one, is what PKCE exists to make impossible, and a server that
    // accepted anything would let that regression through silently.
    Mock::given(method("POST"))
        .and(path("/consumers/oauth2/v2.0/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code_verifier="))
        .and(body_string_contains("code=THE-AUTH-CODE"))
        .respond_with(ResponseTemplate::new(200).set_body_string(TOKEN))
        .mount(&server)
        .await;
    mount_chain(&server).await;

    let provider = provider(&server, Arc::new(MemoryStore::new()));
    drive_browser_login(&provider, |redirect, state| {
        format!("{redirect}/?code=THE-AUTH-CODE&state={state}")
    })
    .await
    .expect("the redemption must carry the verifier");

    let redemption = server
        .received_requests()
        .await
        .expect("requests")
        .into_iter()
        .find(|request| request.url.path().ends_with("/token"))
        .expect("a token request was made");
    let body = String::from_utf8_lossy(&redemption.body).into_owned();

    assert!(body.contains("code_verifier="), "{body}");
    assert!(
        !body.contains("client_secret"),
        "a public client must not send a secret: {body}"
    );
    // The challenge belongs in the authorization request, not the redemption.
    assert!(!body.contains("code_challenge"), "{body}");
}

#[tokio::test]
async fn the_redirect_uri_sent_to_microsoft_names_localhost_and_is_reachable() {
    // The regression this exists for: the code sent `http://127.0.0.1:<port>`
    // while every document told users to register `http://localhost`, and the
    // first real sign-in died on `invalid_request: The provided value for the
    // input parameter 'redirect_uri' is not valid`. Nothing caught it because
    // the browser stand-in below uses whatever URI the code emits, so a wrong
    // host round-trips happily.
    //
    // Both halves are asserted here: the host actually sent, and that a
    // connection to that exact URI reaches the listener. Microsoft only ignores
    // the port for `localhost`, so the host is what makes an ephemeral port
    // legal at all.
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_chain(&server).await;

    let seen: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    let recorder = Arc::clone(&seen);

    let (observer, mut events) = AuthObserver::channel();
    let browser = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let AuthEvent::OpenBrowser { url } = event {
                let parsed = reqwest::Url::parse(&url).expect("an authorization url");
                let query: std::collections::HashMap<_, _> = parsed
                    .query_pairs()
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect();
                let redirect = query["redirect_uri"].clone();
                recorder
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .replace(redirect.clone());
                // Delivered to the redirect URI verbatim: if the listener were
                // not answering on whatever `localhost` resolves to, this send
                // would fail and the login would time out.
                let _ = reqwest::Client::new()
                    .get(format!(
                        "{redirect}/?code=THE-AUTH-CODE&state={}",
                        query["state"]
                    ))
                    .send()
                    .await;
                return;
            }
        }
    });

    let session = provider(&server, Arc::new(MemoryStore::new()))
        .login(
            &LoginRequest {
                flow: LoginFlow::Browser,
                timeout: Duration::from_secs(10),
            },
            &observer,
            &CancellationToken::new(),
        )
        .await;
    drop(observer);
    browser.await.expect("the browser stand-in finished");
    session
        .map(|login| login.session)
        .expect("the redirect must reach the listener");

    let redirect = seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("an authorization request was made");

    assert!(
        redirect.starts_with("http://localhost:"),
        "Microsoft wildcards the port only for `localhost`; got {redirect}"
    );
    assert!(
        !redirect.contains("127.0.0.1"),
        "the IP literal needs a manifest edit and a pinned port: {redirect}"
    );

    // The token redemption must repeat the identical string, or Microsoft
    // rejects the exchange even though the authorization succeeded.
    let redemption = server
        .received_requests()
        .await
        .expect("requests")
        .into_iter()
        .find(|request| request.url.path().ends_with("/token"))
        .expect("a token request was made");
    let body = String::from_utf8_lossy(&redemption.body).into_owned();
    let encoded = redirect.replace(':', "%3A").replace('/', "%2F");
    assert!(
        body.contains(&encoded),
        "the redemption must repeat the same redirect_uri: {body}"
    );
}

#[tokio::test]
async fn a_redirect_carrying_a_forged_state_is_refused_and_the_real_login_still_completes() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_chain(&server).await;

    let provider = provider(&server, Arc::new(MemoryStore::new()));
    let session = drive_browser_login(&provider, |redirect, state| {
        // A stray local process reaches the listener before the browser does.
        let authority = redirect.trim_start_matches("http://");
        let mut stray = std::net::TcpStream::connect(authority).expect("connect to the listener");
        std::io::Write::write_all(
            &mut stray,
            b"GET /?code=STOLEN&state=forged-by-someone-else HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .expect("send the forged redirect");
        format!("{redirect}/?code=REAL&state={state}")
    })
    .await
    .expect("a forged redirect must not end the real login");
    drop(session);

    // Only the code that carried the right state was redeemed.
    let bodies: Vec<String> = server
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .filter(|request| request.url.path().ends_with("/token"))
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect();
    assert!(!bodies.is_empty(), "the real code must be redeemed");
    assert!(
        bodies.iter().all(|body| !body.contains("STOLEN")),
        "the forged code must never reach the token endpoint: {bodies:?}"
    );
}

#[tokio::test]
async fn a_user_who_refuses_consent_is_reported_as_a_refusal_not_a_fault() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_chain(&server).await;

    let provider = provider(&server, Arc::new(MemoryStore::new()));
    let error = drive_browser_login(&provider, |redirect, state| {
        format!("{redirect}/?error=access_denied&error_description=canceled&state={state}")
    })
    .await
    .expect_err("the user said no");
    assert!(matches!(error, AuthError::Declined), "{error:?}");
    assert!(
        !error.requires_interactive_login(),
        "not a loop back to login"
    );
}

/// Drives the browser flow like `drive_browser_login` and also returns the
/// page the browser was shown for the redirect.
async fn login_and_capture_callback_page(
    provider: &MicrosoftAccountProvider,
) -> (Result<ephervex_auth::Login, AuthError>, String) {
    let (observer, mut events) = AuthObserver::channel();
    let browser = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let AuthEvent::OpenBrowser { url } = event {
                let parsed = reqwest::Url::parse(&url).expect("an authorization url");
                let query: std::collections::HashMap<_, _> = parsed
                    .query_pairs()
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
                let target = format!(
                    "{}/?code=THE-AUTH-CODE&state={}",
                    query["redirect_uri"], query["state"]
                );
                let response = reqwest::Client::new()
                    .get(&target)
                    .send()
                    .await
                    .expect("the listener answers the redirect");
                return response.text().await.expect("the page body");
            }
        }
        panic!("the login never asked for a browser");
    });

    let login = provider
        .login(
            &LoginRequest {
                flow: LoginFlow::Browser,
                timeout: Duration::from_secs(10),
            },
            &observer,
            &CancellationToken::new(),
        )
        .await;
    (login, browser.await.expect("the browser stand-in finished"))
}

#[tokio::test]
async fn the_callback_page_does_not_claim_a_sign_in_that_the_token_exchange_then_rejects() {
    let server = MockServer::start().await;
    mount_token(
        &server,
        400,
        r#"{"error":"invalid_grant","error_description":"The code has expired."}"#,
    )
    .await;
    mount_chain(&server).await;

    let provider = provider(&server, Arc::new(MemoryStore::new()));
    let (login, page) = login_and_capture_callback_page(&provider).await;

    assert!(login.is_err(), "the redemption was rejected");
    let page = page.to_ascii_lowercase();
    assert!(!page.contains("signed in"), "{page}");
    assert!(!page.contains("success"), "{page}");
    assert!(!page.contains("the-auth-code"), "{page}");
}

#[tokio::test]
async fn the_callback_page_is_the_same_whether_the_chain_later_succeeds_or_fails() {
    let failing = MockServer::start().await;
    mount_token(&failing, 400, r#"{"error":"invalid_grant"}"#).await;
    mount_chain(&failing).await;
    let (failed, failed_page) =
        login_and_capture_callback_page(&provider(&failing, Arc::new(MemoryStore::new()))).await;
    assert!(failed.is_err());

    let working = MockServer::start().await;
    mount_token(&working, 200, TOKEN).await;
    mount_chain(&working).await;
    let (completed, completed_page) =
        login_and_capture_callback_page(&provider(&working, Arc::new(MemoryStore::new()))).await;
    assert!(completed.is_ok());

    // The page is sent before the outcome is known, so it can only describe
    // what the browser did.
    assert_eq!(failed_page, completed_page);
}

#[tokio::test]
async fn the_device_code_flow_polls_until_the_user_finishes() {
    let server = MockServer::start().await;
    mount_device_code(&server).await;
    mount_chain(&server).await;

    // Two pending answers, then success. `authorization_pending` arrives with a
    // 400 status, so a client that treated any 4xx as fatal would give up on
    // the first poll - which is the bug this ordering exists to catch.
    Mock::given(method("POST"))
        .and(path("/consumers/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(400).set_body_string(PENDING))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/consumers/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(200).set_body_string(TOKEN))
        .mount(&server)
        .await;

    let store = Arc::new(MemoryStore::new());
    let (observer, mut events) = AuthObserver::channel();
    let provider = provider(&server, store.clone());
    let login = provider
        .login(
            &LoginRequest {
                flow: LoginFlow::DeviceCode,
                timeout: Duration::from_secs(10),
            },
            &observer,
            &CancellationToken::new(),
        )
        .await
        .expect("the device flow completes");
    keep(&provider, &login);
    let session = login.session;

    assert_eq!(session.account.id, account());

    // The user has to be told what to type and where; a device flow that does
    // not surface the code cannot be completed by anyone.
    // Stage transitions arrive first; keep draining until the prompt.
    let prompt = loop {
        if let AuthEvent::DeviceCode(prompt) = events.try_recv().expect("an event") {
            break prompt;
        }
    };
    assert_eq!(prompt.user_code, "A1B2C3D4");
    assert_eq!(prompt.verification_uri, "https://microsoft.com/devicelogin");
    assert!(prompt.message.contains("A1B2C3D4"));
}

#[tokio::test]
async fn the_device_code_request_carries_no_redirect_uri() {
    // Verified against Microsoft Learn on 2026-08-08: the device authorization
    // request takes `client_id` and `scope` only. Sending a redirect URI would
    // be sending a parameter the endpoint does not document.
    let server = MockServer::start().await;
    mount_device_code(&server).await;
    mount_token(&server, 400, DECLINED).await;

    let _ = provider(&server, Arc::new(MemoryStore::new()))
        .login(
            &LoginRequest {
                flow: LoginFlow::DeviceCode,
                timeout: Duration::from_secs(5),
            },
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await;

    let request = first_request(&server, "/consumers/oauth2/v2.0/devicecode").await;
    let body = String::from_utf8_lossy(&request.body).into_owned();
    assert!(body.contains("client_id="), "{body}");
    assert!(body.contains("scope="), "{body}");
    assert!(!body.contains("redirect_uri"), "{body}");
    assert!(!body.contains("client_secret"), "{body}");
}

#[tokio::test]
async fn a_declined_device_code_stops_polling_immediately() {
    let server = MockServer::start().await;
    mount_device_code(&server).await;
    Mock::given(method("POST"))
        .and(path("/consumers/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(400).set_body_string(DECLINED))
        // Exactly once: a client that kept polling after a refusal would hammer
        // Microsoft until the code expired.
        .expect(1)
        .mount(&server)
        .await;

    let error = provider(&server, Arc::new(MemoryStore::new()))
        .login(
            &LoginRequest {
                flow: LoginFlow::DeviceCode,
                timeout: Duration::from_secs(5),
            },
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the user declined");
    assert!(matches!(error, AuthError::Declined), "{error:?}");
}

#[tokio::test]
async fn an_expired_device_code_stops_polling_and_says_so() {
    let server = MockServer::start().await;
    mount_device_code(&server).await;
    Mock::given(method("POST"))
        .and(path("/consumers/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(400).set_body_string(EXPIRED))
        .expect(1)
        .mount(&server)
        .await;

    let error = provider(&server, Arc::new(MemoryStore::new()))
        .login(
            &LoginRequest {
                flow: LoginFlow::DeviceCode,
                timeout: Duration::from_secs(5),
            },
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the fifteen minutes ran out");
    assert!(matches!(error, AuthError::DeviceCodeExpired), "{error:?}");
}

#[tokio::test]
async fn a_device_code_login_ends_at_the_caller_timeout_while_the_user_has_not_finished() {
    let server = MockServer::start().await;
    mount_device_code(&server).await;
    mount_token(&server, 400, PENDING).await;

    // Longer than the one-second poll interval, so the deadline lands while the
    // login is sleeping between polls rather than before the first one.
    let started = std::time::Instant::now();
    let error = provider(&server, Arc::new(MemoryStore::new()))
        .login(
            &LoginRequest {
                flow: LoginFlow::DeviceCode,
                timeout: Duration::from_millis(1500),
            },
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("nobody finished within the caller's deadline");
    assert!(matches!(error, AuthError::TimedOut { .. }), "{error:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_device_code_login_ends_when_the_reported_code_lifetime_runs_out() {
    let server = MockServer::start().await;
    mount_device_code_with_lifetime(&server, Some(2)).await;
    mount_token(&server, 400, PENDING).await;

    // The service never answers `expired_token` here; the lifetime it reported
    // is the only thing that can end the wait before the caller's deadline.
    let started = std::time::Instant::now();
    let error = provider(&server, Arc::new(MemoryStore::new()))
        .login(
            &LoginRequest {
                flow: LoginFlow::DeviceCode,
                timeout: Duration::from_secs(300),
            },
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the code outlived its reported lifetime");
    assert!(matches!(error, AuthError::DeviceCodeExpired), "{error:?}");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn an_unrepresentable_timeout_or_code_lifetime_leaves_the_other_bound_in_force() {
    // Neither value is trusted: one is a command-line number, the other a
    // service-supplied field. Either one being enormous must not panic the
    // clock arithmetic or lift the bound the other one provides.
    let server = MockServer::start().await;
    mount_token(&server, 400, PENDING).await;
    mount_device_code_with_lifetime(&server, Some(2)).await;
    let error = provider(&server, Arc::new(MemoryStore::new()))
        .login(
            &LoginRequest {
                flow: LoginFlow::DeviceCode,
                timeout: Duration::from_secs(u64::MAX),
            },
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the code lifetime still ends the wait");
    assert!(matches!(error, AuthError::DeviceCodeExpired), "{error:?}");

    server.reset().await;
    mount_token(&server, 400, PENDING).await;
    mount_device_code_with_lifetime(&server, Some(i64::MAX as u64)).await;
    let error = provider(&server, Arc::new(MemoryStore::new()))
        .login(
            &LoginRequest {
                flow: LoginFlow::DeviceCode,
                timeout: Duration::from_millis(300),
            },
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the caller timeout still ends the wait");
    assert!(matches!(error, AuthError::TimedOut { .. }), "{error:?}");
}

#[tokio::test]
async fn a_device_code_login_can_still_be_canceled_while_waiting() {
    let server = MockServer::start().await;
    mount_device_code(&server).await;
    mount_token(&server, 400, PENDING).await;

    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        trigger.cancel();
    });
    let error = provider(&server, Arc::new(MemoryStore::new()))
        .login(
            &LoginRequest {
                flow: LoginFlow::DeviceCode,
                timeout: Duration::from_secs(300),
            },
            &AuthObserver::silent(),
            &cancel,
        )
        .await
        .expect_err("the caller canceled");
    assert!(matches!(error, AuthError::Canceled), "{error:?}");
}

#[tokio::test]
async fn a_device_code_response_without_a_lifetime_is_refused() {
    let server = MockServer::start().await;
    mount_device_code_with_lifetime(&server, None).await;

    let error = provider(&server, Arc::new(MemoryStore::new()))
        .login(
            &LoginRequest {
                flow: LoginFlow::DeviceCode,
                timeout: Duration::from_secs(5),
            },
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("RFC 8628 requires expires_in");
    assert!(
        matches!(error, AuthError::MalformedResponse { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_device_code_lifetime_beyond_the_clock_range_does_not_panic_the_prompt() {
    let server = MockServer::start().await;
    mount_token(&server, 400, PENDING).await;
    mount_device_code_with_lifetime(&server, Some(9_000_000_000_000)).await;

    let error = provider(&server, Arc::new(MemoryStore::new()))
        .login(
            &LoginRequest {
                flow: LoginFlow::DeviceCode,
                timeout: Duration::from_millis(300),
            },
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the caller timeout still ends the wait");
    assert!(matches!(error, AuthError::TimedOut { .. }), "{error:?}");
}

#[tokio::test]
async fn a_microsoft_token_lifetime_beyond_the_clock_range_does_not_panic_sign_in() {
    let server = MockServer::start().await;
    let mut token: serde_json::Value = serde_json::from_str(TOKEN).expect("fixture parses");
    token["expires_in"] = 9_000_000_000_000_i64.into();
    mount_token(&server, 200, &token.to_string()).await;
    mount_chain(&server).await;

    drive_browser_login(
        &provider(&server, Arc::new(MemoryStore::new())),
        |redirect, state| format!("{redirect}/?code=THE-AUTH-CODE&state={state}"),
    )
    .await
    .expect("the Microsoft token lifetime does not decide the session");
}

/// The Minecraft login document with `expires_in` replaced, or removed.
fn minecraft_login_with_lifetime(expires_in: Option<i64>) -> String {
    let mut body: serde_json::Value = serde_json::from_str(MC_LOGIN).expect("fixture parses");
    match expires_in {
        Some(seconds) => body["expires_in"] = seconds.into(),
        None => {
            body.as_object_mut().expect("object").remove("expires_in");
        }
    }
    body.to_string()
}

#[tokio::test]
async fn a_minecraft_login_without_a_usable_lifetime_is_refused_rather_than_reported_as_a_session()
{
    for expires_in in [None, Some(0), Some(-60)] {
        let server = MockServer::start().await;
        mount_token(&server, 200, TOKEN).await;
        mount_xbox(&server, 200, XBOX).await;
        mount_xsts(&server, 200, XSTS).await;
        mount_minecraft_login(&server, 200, &minecraft_login_with_lifetime(expires_in)).await;
        mount_entitlements(&server, 200, ENTITLEMENTS).await;
        mount_profile(&server, 200, PROFILE).await;

        let store = Arc::new(MemoryStore::new());
        let error = drive_browser_login(&provider(&server, store.clone()), |redirect, state| {
            format!("{redirect}/?code=THE-AUTH-CODE&state={state}")
        })
        .await
        .expect_err("a token with no time left is not a login");
        assert!(
            matches!(
                error,
                AuthError::MalformedResponse {
                    stage: AuthStage::MinecraftLogin,
                    ..
                }
            ),
            "{expires_in:?}: {error:?}"
        );
        assert!(
            store
                .load(&CredentialKey::MicrosoftRefreshToken(account()))
                .expect("load")
                .is_none(),
            "{expires_in:?}: nothing is stored for a login that failed"
        );
    }
}

#[tokio::test]
async fn a_minecraft_lifetime_beyond_the_clock_range_does_not_panic_and_yields_a_usable_session() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_xbox(&server, 200, XBOX).await;
    mount_xsts(&server, 200, XSTS).await;
    mount_minecraft_login(
        &server,
        200,
        &minecraft_login_with_lifetime(Some(9_000_000_000_000)),
    )
    .await;
    mount_entitlements(&server, 200, ENTITLEMENTS).await;
    mount_profile(&server, 200, PROFILE).await;

    let session = drive_browser_login(
        &provider(&server, Arc::new(MemoryStore::new())),
        |redirect, state| format!("{redirect}/?code=THE-AUTH-CODE&state={state}"),
    )
    .await
    .expect("a very long lifetime is bounded, not fatal");
    assert!(session.is_usable(chrono::TimeDelta::seconds(60)));
}

// ------------------------------------------------------------------- refresh

#[tokio::test]
async fn resuming_refreshes_the_token_and_stores_the_rotated_credential() {
    let server = MockServer::start().await;
    // Microsoft may replace the refresh token on any redemption and documents
    // that the client must discard the old one. A resume that kept the original
    // works for weeks and then strands the user.
    Mock::given(method("POST"))
        .and(path("/consumers/oauth2/v2.0/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(200).set_body_string(TOKEN_ROTATED))
        .mount(&server)
        .await;
    mount_chain(&server).await;

    let store = Arc::new(MemoryStore::new());
    store
        .store(
            &CredentialKey::MicrosoftRefreshToken(account()),
            &SecretString::new("FAKE-MICROSOFT-REFRESH-TOKEN"),
        )
        .expect("seed a previous login");

    let session = provider(&server, store.clone())
        .resume(
            account(),
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect("a stored credential resumes");

    assert_eq!(session.account.id, account());
    assert_eq!(
        store
            .load(&CredentialKey::MicrosoftRefreshToken(account()))
            .expect("load")
            .expect("present")
            .expose(),
        "FAKE-MICROSOFT-REFRESH-TOKEN-ROTATED",
        "the replacement must overwrite the old credential"
    );
}

#[tokio::test]
async fn a_rotated_refresh_token_survives_a_failure_further_down_the_chain() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN_ROTATED).await;
    mount_xbox(&server, 503, "").await;

    let store = Arc::new(MemoryStore::new());
    let key = CredentialKey::MicrosoftRefreshToken(account());
    store
        .store(&key, &SecretString::new("FAKE-MICROSOFT-REFRESH-TOKEN"))
        .expect("seed");

    let error = provider(&server, store.clone())
        .resume(
            account(),
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("xbox live is down");

    assert!(error.is_transient(), "{error:?}");
    assert_eq!(
        store.load(&key).expect("load").expect("present").expose(),
        "FAKE-MICROSOFT-REFRESH-TOKEN-ROTATED",
        "the replacement is the credential to keep once Microsoft issued it"
    );
}

#[tokio::test]
async fn a_resume_that_signs_in_as_another_account_is_refused_and_stores_nothing_for_it() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN_ROTATED).await;
    mount_xbox(&server, 200, XBOX).await;
    mount_xsts(&server, 200, XSTS).await;
    mount_minecraft_login(&server, 200, MC_LOGIN).await;
    mount_entitlements(&server, 200, ENTITLEMENTS).await;
    let other = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";
    mount_profile(
        &server,
        200,
        &PROFILE.replace("069a79f444e94726a5befca90e38aaf5", other),
    )
    .await;

    let store = Arc::new(MemoryStore::new());
    let key = CredentialKey::MicrosoftRefreshToken(account());
    store
        .store(&key, &SecretString::new("FAKE-MICROSOFT-REFRESH-TOKEN"))
        .expect("seed");

    let error = provider(&server, store.clone())
        .resume(
            account(),
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the credential belongs to someone else");

    let other = Uuid::parse_str(other).expect("uuid");
    assert!(
        matches!(
            error,
            AuthError::AccountMismatch { requested, found }
                if requested == account() && found == other
        ),
        "{error:?}"
    );
    assert!(error.requires_interactive_login());
    assert!(
        store
            .load(&CredentialKey::MicrosoftRefreshToken(other))
            .expect("load")
            .is_none(),
        "nothing is stored under the identity the profile claimed"
    );
}

/// Starts a resume on its own task and returns once it reports `stage`.
///
/// The token endpoint answers slowly enough that the caller acts while the
/// resume is still waiting on Microsoft.
async fn resume_until(
    provider: &MicrosoftAccountProvider,
    stage: ephervex_auth::AuthStage,
) -> tokio::task::JoinHandle<Result<ephervex_auth::Session, AuthError>> {
    let (observer, mut events) = AuthObserver::channel();
    let provider = provider.clone();
    let task = tokio::spawn(async move {
        provider
            .resume(account(), &observer, &CancellationToken::new())
            .await
    });
    while let Some(event) = events.recv().await {
        if matches!(event, AuthEvent::Stage(reached) if reached == stage) {
            break;
        }
    }
    task
}

#[tokio::test]
async fn a_logout_during_the_token_refresh_is_not_undone_by_the_replacement() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/consumers/oauth2/v2.0/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(TOKEN_ROTATED)
                .set_delay(Duration::from_millis(400)),
        )
        .mount(&server)
        .await;
    mount_chain(&server).await;

    let store = Arc::new(MemoryStore::new());
    let key = CredentialKey::MicrosoftRefreshToken(account());
    store
        .store(&key, &SecretString::new("FAKE-MICROSOFT-REFRESH-TOKEN"))
        .expect("seed");
    let provider = provider(&server, store.clone());

    let resume = resume_until(&provider, ephervex_auth::AuthStage::Microsoft).await;
    provider.logout(account()).expect("logout");
    let error = resume
        .await
        .expect("task")
        .expect_err("the account signed out meanwhile");

    assert!(matches!(error, AuthError::NotSignedIn), "{error:?}");
    assert!(store.load(&key).expect("load").is_none());
}

#[tokio::test]
async fn a_logout_during_the_later_legs_leaves_no_credential_and_no_session() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN_ROTATED).await;
    mount_xbox(&server, 200, XBOX).await;
    mount_xsts(&server, 200, XSTS).await;
    mount_minecraft_login(&server, 200, MC_LOGIN).await;
    mount_entitlements(&server, 200, ENTITLEMENTS).await;
    Mock::given(method("GET"))
        .and(path("/minecraft/profile"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(PROFILE)
                .set_delay(Duration::from_millis(400)),
        )
        .mount(&server)
        .await;

    let store = Arc::new(MemoryStore::new());
    let key = CredentialKey::MicrosoftRefreshToken(account());
    store
        .store(&key, &SecretString::new("FAKE-MICROSOFT-REFRESH-TOKEN"))
        .expect("seed");
    let provider = provider(&server, store.clone());

    let resume = resume_until(&provider, ephervex_auth::AuthStage::Profile).await;
    provider.logout(account()).expect("logout");
    let error = resume
        .await
        .expect("task")
        .expect_err("the account signed out meanwhile");

    assert!(matches!(error, AuthError::NotSignedIn), "{error:?}");
    assert!(store.load(&key).expect("load").is_none());
}

/// A store that holds the first `store` call until released, so a test can act
/// while a resume is part-way through publishing its replacement.
#[derive(Debug)]
struct StoreHeldOpen {
    inner: MemoryStore,
    entered: std::sync::Mutex<std::sync::mpsc::Sender<()>>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl CredentialStore for StoreHeldOpen {
    fn store(&self, key: &CredentialKey, secret: &SecretString) -> Result<(), AuthError> {
        self.entered.lock().unwrap().send(()).unwrap();
        self.release.lock().unwrap().recv().unwrap();
        self.inner.store(key, secret)
    }

    fn load(&self, key: &CredentialKey) -> Result<Option<SecretString>, AuthError> {
        self.inner.load(key)
    }

    fn delete(&self, key: &CredentialKey) -> Result<CredentialRemoval, AuthError> {
        self.inner.delete(key)
    }

    fn storage(&self) -> CredentialStorage {
        self.inner.storage()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removal_made_outside_the_provider_waits_for_a_resume_that_is_publishing() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN_ROTATED).await;
    mount_chain(&server).await;

    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let store = Arc::new(StoreHeldOpen {
        inner: MemoryStore::new(),
        entered: std::sync::Mutex::new(entered_tx),
        release: std::sync::Mutex::new(release_rx),
    });
    let key = CredentialKey::MicrosoftRefreshToken(account());
    store
        .inner
        .store(&key, &SecretString::new("FAKE-MICROSOFT-REFRESH-TOKEN"))
        .expect("seed");
    let provider = provider(&server, store.clone());

    let resume = tokio::spawn(async move {
        provider
            .resume(
                account(),
                &AuthObserver::silent(),
                &CancellationToken::new(),
            )
            .await
    });
    tokio::task::spawn_blocking(move || entered_rx.recv().expect("the resume reaches the store"))
        .await
        .expect("task");

    // What account removal does: a deletion that is not the provider's own.
    let removal = {
        let store = store.clone();
        let key = key.clone();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        let handle = std::thread::spawn(move || {
            let _ordered = ProcessChangeOrder.hold().expect("order");
            store.delete(&key).expect("delete");
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        (handle, done)
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !removal.1.load(std::sync::atomic::Ordering::SeqCst),
        "the removal must not land between the resume's check and its store"
    );

    release_tx.send(()).expect("release");
    removal.0.join().expect("removal");
    let _ = resume.await.expect("task");

    assert!(
        store.load(&key).expect("load").is_none(),
        "a resume that was publishing must not bring the removed credential back"
    );
}

/// An order that never lets a change proceed, as a lock held by another
/// process for longer than a change waits.
#[derive(Debug)]
struct OrderHeldElsewhere;

impl CredentialChangeOrder for OrderHeldElsewhere {
    fn hold(&self) -> Result<ephervex_auth::CredentialChangeGuard, AuthError> {
        Err(AuthError::CredentialChangeBusy {
            path: std::path::PathBuf::from("credentials.lock"),
        })
    }
}

#[tokio::test]
async fn a_resume_stores_nothing_when_the_supplied_order_cannot_be_taken() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN_ROTATED).await;
    mount_chain(&server).await;
    let store = Arc::new(MemoryStore::new());
    let key = CredentialKey::MicrosoftRefreshToken(account());
    store
        .store(&key, &SecretString::new("FAKE-MICROSOFT-REFRESH-TOKEN"))
        .expect("seed");

    let error = provider(&server, store.clone())
        .with_change_order(Arc::new(OrderHeldElsewhere))
        .resume(
            account(),
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("another process is changing credentials");

    assert!(
        matches!(error, AuthError::CredentialChangeBusy { .. }),
        "{error:?}"
    );
    assert_eq!(
        store.load(&key).expect("load").expect("kept").expose(),
        "FAKE-MICROSOFT-REFRESH-TOKEN",
        "the rotated credential is not stored outside the order"
    );
}

#[tokio::test]
async fn a_rejected_credential_is_not_removed_once_a_newer_one_replaced_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/consumers/oauth2/v2.0/token"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string(INVALID_GRANT)
                .set_delay(Duration::from_millis(400)),
        )
        .mount(&server)
        .await;

    let store = Arc::new(MemoryStore::new());
    let key = CredentialKey::MicrosoftRefreshToken(account());
    store
        .store(&key, &SecretString::new("FAKE-EXPIRED-REFRESH-TOKEN"))
        .expect("seed");
    let provider = provider(&server, store.clone());

    let resume = resume_until(&provider, ephervex_auth::AuthStage::Microsoft).await;
    store
        .store(&key, &SecretString::new("FAKE-NEWER-REFRESH-TOKEN"))
        .expect("a newer sign-in");
    let error = resume
        .await
        .expect("task")
        .expect_err("the old credential is dead");

    assert!(
        matches!(error, AuthError::ReauthenticationRequired { .. }),
        "{error:?}"
    );
    assert_eq!(
        store.load(&key).expect("load").expect("kept").expose(),
        "FAKE-NEWER-REFRESH-TOKEN"
    );
}

#[tokio::test]
async fn an_expired_refresh_token_asks_for_a_login_and_clears_the_dead_credential() {
    let server = MockServer::start().await;
    mount_token(&server, 400, INVALID_GRANT).await;

    let store = Arc::new(MemoryStore::new());
    store
        .store(
            &CredentialKey::MicrosoftRefreshToken(account()),
            &SecretString::new("FAKE-EXPIRED-REFRESH-TOKEN"),
        )
        .expect("seed");

    let error = provider(&server, store.clone())
        .resume(
            account(),
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("the credential is dead");

    assert!(
        matches!(error, AuthError::ReauthenticationRequired { .. }),
        "{error:?}"
    );
    assert!(
        error.requires_interactive_login(),
        "Vanilla launch support turns this into LaunchError::AuthenticationRequired"
    );
    // Leaving it behind would make `account status` insist a credential exists
    // while every resume failed identically.
    assert!(
        store
            .load(&CredentialKey::MicrosoftRefreshToken(account()))
            .expect("load")
            .is_none(),
        "a rejected credential must not be left in the store"
    );
}

#[tokio::test]
async fn a_cross_origin_redirect_never_replays_the_refresh_token_to_the_other_origin() {
    for status in [307_u16, 308] {
        let server = MockServer::start().await;
        let elsewhere = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(TOKEN))
            .mount(&elsewhere)
            .await;
        Mock::given(method("POST"))
            .and(path("/consumers/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(status).insert_header(
                "location",
                format!("{}/consumers/oauth2/v2.0/token", elsewhere.uri()),
            ))
            .mount(&server)
            .await;
        mount_chain(&server).await;

        let store = Arc::new(MemoryStore::new());
        store
            .store(
                &CredentialKey::MicrosoftRefreshToken(account()),
                &SecretString::new("FAKE-MICROSOFT-REFRESH-TOKEN"),
            )
            .expect("seed a previous login");

        let error = provider(&server, store)
            .resume(
                account(),
                &AuthObserver::silent(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("a redirect off the origin is a failure, not a login");

        assert!(
            matches!(error, AuthError::HttpStatus { status: refused, .. } if refused == status),
            "{status}: {error:?}"
        );
        assert!(
            elsewhere
                .received_requests()
                .await
                .expect("requests")
                .is_empty(),
            "{status}: the credential body must not reach another origin"
        );
    }
}

#[tokio::test]
async fn a_redirect_that_stays_on_the_same_origin_is_still_followed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/consumers/oauth2/v2.0/token"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/token-moved", server.uri())),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token-moved"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(200).set_body_string(TOKEN_ROTATED))
        .mount(&server)
        .await;
    mount_chain(&server).await;

    let store = Arc::new(MemoryStore::new());
    store
        .store(
            &CredentialKey::MicrosoftRefreshToken(account()),
            &SecretString::new("FAKE-MICROSOFT-REFRESH-TOKEN"),
        )
        .expect("seed a previous login");

    provider(&server, store)
        .resume(
            account(),
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect("a same-origin redirect reaches the host the credential was issued for");
}

#[tokio::test]
async fn logging_out_removes_the_stored_credential() {
    let server = MockServer::start().await;
    let store = Arc::new(MemoryStore::new());
    let provider = provider(&server, store.clone());

    store
        .store(
            &CredentialKey::MicrosoftRefreshToken(account()),
            &SecretString::new("FAKE-MICROSOFT-REFRESH-TOKEN"),
        )
        .expect("seed");
    assert!(provider.has_stored_credential(account()).expect("query"));

    provider.logout(account()).expect("logout");
    assert!(!provider.has_stored_credential(account()).expect("query"));
    // ADR 0004: removal, not forgetting. A second logout is still fine.
    provider.logout(account()).expect("logout is idempotent");
}

// --------------------------------------------------------------- refusals

#[tokio::test]
async fn an_account_that_does_not_own_the_game_is_refused_before_a_session_exists() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_xbox(&server, 200, XBOX).await;
    mount_xsts(&server, 200, XSTS).await;
    mount_minecraft_login(&server, 200, MC_LOGIN).await;
    mount_entitlements(&server, 200, ENTITLEMENTS_NONE).await;
    mount_profile(&server, 404, PROFILE_MISSING).await;

    let store = Arc::new(MemoryStore::new());
    let error = drive_browser_login(&provider(&server, store.clone()), |redirect, state| {
        format!("{redirect}/?code=THE-AUTH-CODE&state={state}")
    })
    .await
    .expect_err("no profile means no launch");

    match &error {
        AuthError::GameNotOwned {
            entitlements,
            game_pass,
        } => {
            assert!(entitlements.is_empty());
            assert!(!game_pass);
        }
        other => panic!("unexpected: {other:?}"),
    }
    assert_eq!(error.message_id(), "error-auth-game-not-owned");

    // Ownership is verified *before* launch. Nothing was stored, so
    // there is no credential a later resume could use to skip the check.
    assert!(
        store
            .load(&CredentialKey::MicrosoftRefreshToken(account()))
            .expect("load")
            .is_none()
    );
}

#[tokio::test]
async fn a_game_pass_account_with_no_profile_gets_a_different_answer() {
    // This is the case that is easy to get wrong. The account *is* entitled;
    // it simply has never run the official launcher, so no Java Edition profile
    // exists yet. Telling this user they do not own Minecraft would be false.
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_xbox(&server, 200, XBOX).await;
    mount_xsts(&server, 200, XSTS).await;
    mount_minecraft_login(&server, 200, MC_LOGIN).await;
    mount_entitlements(&server, 200, ENTITLEMENTS_GAME_PASS).await;
    mount_profile(&server, 404, PROFILE_MISSING).await;

    let error = drive_browser_login(
        &provider(&server, Arc::new(MemoryStore::new())),
        |redirect, state| format!("{redirect}/?code=THE-AUTH-CODE&state={state}"),
    )
    .await
    .expect_err("still no profile to launch with");

    match &error {
        AuthError::GameNotOwned {
            game_pass,
            entitlements,
        } => {
            assert!(game_pass, "the store reported a Game Pass entitlement");
            assert_eq!(entitlements, &["product_game_pass_ultimate"]);
        }
        other => panic!("unexpected: {other:?}"),
    }
    assert_eq!(error.message_id(), "error-auth-game-pass-setup-required");
}

#[tokio::test]
async fn an_unapproved_azure_application_is_told_to_apply_rather_than_to_retry() {
    // Microsoft requires a new Azure application to be approved before it may
    // call `api.minecraftservices.com`; until then every endpoint there answers
    // 403. This must not become `MinecraftLoginRejected`, whose retry advice
    // can never help because approval requires an external application form.
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_xbox(&server, 200, XBOX).await;
    mount_xsts(&server, 200, XSTS).await;
    mount_minecraft_login(&server, 403, "").await;

    let error = drive_browser_login(
        &provider(&server, Arc::new(MemoryStore::new())),
        |redirect, state| format!("{redirect}/?code=THE-AUTH-CODE&state={state}"),
    )
    .await
    .expect_err("an unapproved application cannot sign anyone in");

    match &error {
        AuthError::MinecraftApiNotApproved { client_id } => {
            // The approval form asks for the Client ID, so the error carries
            // the exact value the user has to paste into it.
            assert_eq!(client_id, CLIENT_ID);
        }
        other => panic!("a 403 is not a rejected identity: {other:?}"),
    }

    assert_eq!(error.message_id(), "error-auth-minecraft-api-not-approved");
    assert_eq!(
        error.help_url(),
        Some(ephervex_auth::MINECRAFT_API_REVIEW_FORM)
    );
    assert!(error.needs_app_registration_approval());

    // None of the three things a user might be told to do can help here, and
    // saying any of them would be worse than saying nothing.
    assert!(!error.is_transient(), "waiting cannot fix an approval");
    assert!(
        !error.requires_interactive_login(),
        "a Sign in button would be a button that cannot work"
    );
    assert!(!error.session_only_would_help());
}

#[tokio::test]
async fn a_403_and_a_503_from_minecraft_services_do_not_share_a_message() {
    // The distinction the mapping exists for: one is permanent and one is not.
    let permanent = AuthError::MinecraftApiNotApproved {
        client_id: CLIENT_ID.to_owned(),
    };
    let transient = AuthError::MinecraftLoginRejected { status: 503 };
    let rejected = AuthError::MinecraftLoginRejected { status: 400 };

    assert_ne!(permanent.message_id(), transient.message_id());
    assert_ne!(rejected.message_id(), transient.message_id());
    assert!(transient.is_transient());
    assert!(!rejected.is_transient());
    assert!(!permanent.is_transient());

    // The same split applies on the other two endpoints of the same host,
    // because the same 403 reaches all three.
    assert_eq!(
        AuthError::ProfileUnavailable { status: 503 }.message_id(),
        "error-auth-minecraft-unavailable"
    );
    assert_ne!(
        AuthError::ProfileUnavailable { status: 500 }.message_id(),
        AuthError::ProfileUnavailable { status: 400 }.message_id()
    );
}

#[tokio::test]
async fn a_403_reaches_the_same_conclusion_from_the_profile_endpoint() {
    // If approval ever lands part-way - or a future endpoint is gated
    // separately - the profile call must reach the same conclusion rather than
    // reporting a profile problem.
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_xbox(&server, 200, XBOX).await;
    mount_xsts(&server, 200, XSTS).await;
    mount_minecraft_login(&server, 200, MC_LOGIN).await;
    mount_entitlements(&server, 200, ENTITLEMENTS).await;
    mount_profile(&server, 403, "").await;

    let error = drive_browser_login(
        &provider(&server, Arc::new(MemoryStore::new())),
        |redirect, state| format!("{redirect}/?code=THE-AUTH-CODE&state={state}"),
    )
    .await
    .expect_err("403 is not a missing profile");
    assert!(
        error.needs_app_registration_approval(),
        "a 403 must not be reported as a profile outage: {error:?}"
    );
}

#[tokio::test]
async fn a_403_from_the_entitlement_store_is_not_swallowed_as_an_outage() {
    // The entitlement call tolerates failures on purpose, so that an outage
    // there cannot lock a paying user out. That tolerance must not extend to a
    // 403, which is not an outage and would otherwise be reported one call
    // later under a warning blaming the store.
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_xbox(&server, 200, XBOX).await;
    mount_xsts(&server, 200, XSTS).await;
    mount_minecraft_login(&server, 200, MC_LOGIN).await;
    mount_entitlements(&server, 403, "").await;
    mount_profile(&server, 200, PROFILE).await;

    let error = drive_browser_login(
        &provider(&server, Arc::new(MemoryStore::new())),
        |redirect, state| format!("{redirect}/?code=THE-AUTH-CODE&state={state}"),
    )
    .await
    .expect_err("a 403 is not a tolerable entitlement failure");
    assert!(error.needs_app_registration_approval(), "{error:?}");
}

#[tokio::test]
async fn an_account_with_no_xbox_profile_is_told_which_xbox_problem_it_has() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_xbox(&server, 200, XBOX).await;
    mount_xsts(&server, 401, XSTS_DENIED).await;

    let error = drive_browser_login(
        &provider(&server, Arc::new(MemoryStore::new())),
        |redirect, state| format!("{redirect}/?code=THE-AUTH-CODE&state={state}"),
    )
    .await
    .expect_err("xsts refused");

    match error {
        AuthError::XstsDenied { reason, xerr } => {
            assert_eq!(reason, XstsDenial::NoXboxAccount);
            assert_eq!(xerr, Some(2_148_916_233));
            // "Authorization failed" would send this user looking in the wrong
            // place; what they need is to create an Xbox profile.
            assert_eq!(reason.message_id(), "error-auth-xbox-no-account");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn an_entitlement_store_outage_does_not_lock_a_paying_user_out() {
    // The profile is what decides. If the entitlement call — which exists to
    // *explain* a refusal — is what fails, refusing the login would be the
    // launcher inventing a reason.
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_xbox(&server, 200, XBOX).await;
    mount_xsts(&server, 200, XSTS).await;
    mount_minecraft_login(&server, 200, MC_LOGIN).await;
    mount_entitlements(&server, 503, "{}").await;
    mount_profile(&server, 200, PROFILE).await;

    let session = drive_browser_login(
        &provider(&server, Arc::new(MemoryStore::new())),
        |redirect, state| format!("{redirect}/?code=THE-AUTH-CODE&state={state}"),
    )
    .await
    .expect("a profile is enough to launch");
    assert_eq!(session.account.id, account());
    // The store did not answer, so nothing is known: not an empty list.
    assert!(session.entitlements().is_none());
    assert!(matches!(
        session.entitlement_lookup(),
        Some(EntitlementLookup::Unavailable)
    ));
}

#[tokio::test]
async fn a_missing_profile_during_an_entitlement_outage_is_not_a_definite_refusal() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_xbox(&server, 200, XBOX).await;
    mount_xsts(&server, 200, XSTS).await;
    mount_minecraft_login(&server, 200, MC_LOGIN).await;
    mount_entitlements(&server, 503, "{}").await;
    mount_profile(&server, 404, PROFILE_MISSING).await;

    let store = Arc::new(MemoryStore::new());
    let error = drive_browser_login(&provider(&server, store.clone()), |redirect, state| {
        format!("{redirect}/?code=THE-AUTH-CODE&state={state}")
    })
    .await
    .expect_err("no profile means no launch");

    assert!(
        matches!(error, AuthError::GameOwnershipUnconfirmed),
        "{error:?}"
    );
    assert_eq!(error.message_id(), "error-auth-game-ownership-unconfirmed");
    assert!(error.is_transient());
    assert!(
        store
            .load(&CredentialKey::MicrosoftRefreshToken(account()))
            .expect("load")
            .is_none()
    );
}

// ------------------------------------------------------- secret handling

#[tokio::test]
async fn session_only_login_works_and_reports_that_it_will_not_survive_a_restart() {
    // When Secret Service is unavailable, session-only login is
    // offered. `MemoryStore` *is* that mode, and the session says so rather
    // than pretending the login is persistent.
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_chain(&server).await;

    let session = drive_browser_login(
        &provider(&server, Arc::new(MemoryStore::new())),
        |redirect, state| format!("{redirect}/?code=THE-AUTH-CODE&state={state}"),
    )
    .await
    .expect("session-only login still logs in");

    assert_eq!(session.storage(), Some(CredentialStorage::SessionOnly));
    assert!(
        !session
            .storage()
            .is_some_and(CredentialStorage::is_persistent),
        "the user must be able to tell this login will not survive a restart"
    );
}

#[tokio::test]
async fn a_missing_secret_service_is_refused_explicitly_rather_than_written_to_disk() {
    // ADR 0004's third rule: never silently fall back to plaintext. There is no
    // file-backed store to fall back *to*, and the refusal a caller sees says
    // that session-only login is the alternative.
    let error = AuthError::SecretStorageUnavailable {
        detail: "no default credential store".to_owned(),
    };
    assert!(error.session_only_would_help());
    assert_eq!(error.message_id(), "error-auth-secret-storage-unavailable");
}

#[tokio::test]
async fn the_xbox_leg_marks_the_ticket_as_coming_from_the_identity_platform() {
    // The `d=` prefix is required for identity-platform tokens; without it Xbox
    // Live answers 400 with nothing useful in it.
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_chain(&server).await;

    drive_browser_login(
        &provider(&server, Arc::new(MemoryStore::new())),
        |redirect, state| format!("{redirect}/?code=THE-AUTH-CODE&state={state}"),
    )
    .await
    .expect("chain completes");

    let request = first_request(&server, "/user/authenticate").await;
    let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json body");
    assert_eq!(
        body["Properties"]["RpsTicket"],
        "d=FAKE-MICROSOFT-ACCESS-TOKEN"
    );
    assert_eq!(body["Properties"]["AuthMethod"], "RPS");
    assert_eq!(body["RelyingParty"], "http://auth.xboxlive.com");
    assert_eq!(body["TokenType"], "JWT");
}

#[tokio::test]
async fn the_xsts_leg_asks_for_a_token_scoped_to_minecraft() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_chain(&server).await;

    drive_browser_login(
        &provider(&server, Arc::new(MemoryStore::new())),
        |redirect, state| format!("{redirect}/?code=THE-AUTH-CODE&state={state}"),
    )
    .await
    .expect("chain completes");

    let request = first_request(&server, "/xsts/authorize").await;
    let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json body");
    // Reusing the Xbox relying party here mints a token Minecraft rejects.
    assert_eq!(body["RelyingParty"], "rp://api.minecraftservices.com/");
    assert_eq!(body["Properties"]["SandboxId"], "RETAIL");
    assert_eq!(body["Properties"]["UserTokens"][0], "FAKE-XBOX-USER-TOKEN");
}

#[tokio::test]
async fn minecraft_services_is_given_the_identity_token_in_the_documented_format() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    Mock::given(method("POST"))
        .and(path("/authentication/login_with_xbox"))
        .and(header("accept", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(MC_LOGIN))
        .mount(&server)
        .await;
    mount_xbox(&server, 200, XBOX).await;
    mount_xsts(&server, 200, XSTS).await;
    mount_entitlements(&server, 200, ENTITLEMENTS).await;
    mount_profile(&server, 200, PROFILE).await;

    drive_browser_login(
        &provider(&server, Arc::new(MemoryStore::new())),
        |redirect, state| format!("{redirect}/?code=THE-AUTH-CODE&state={state}"),
    )
    .await
    .expect("chain completes");

    let request = first_request(&server, "/authentication/login_with_xbox").await;
    let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json body");
    assert_eq!(
        body["identityToken"],
        "XBL3.0 x=1503216055647848448;FAKE-XSTS-TOKEN"
    );
}

#[tokio::test]
async fn the_minecraft_calls_carry_the_bearer_token_and_the_launcher_user_agent() {
    let server = MockServer::start().await;
    mount_token(&server, 200, TOKEN).await;
    mount_chain(&server).await;

    drive_browser_login(
        &provider(&server, Arc::new(MemoryStore::new())),
        |redirect, state| format!("{redirect}/?code=THE-AUTH-CODE&state={state}"),
    )
    .await
    .expect("chain completes");

    let profile = first_request(&server, "/minecraft/profile").await;
    assert_eq!(
        profile
            .headers
            .get("authorization")
            .expect("authorization header"),
        "Bearer FAKE-MINECRAFT-ACCESS-TOKEN"
    );
    // every outbound request identifies the launcher, and there is one
    // client construction point that guarantees it.
    let agent = profile
        .headers
        .get("user-agent")
        .expect("user-agent")
        .to_str()
        .expect("ascii");
    assert!(agent.starts_with("Ephervex/"), "{agent}");
}

/// The first request the server received for a path.
async fn first_request(server: &MockServer, path: &str) -> Request {
    server
        .received_requests()
        .await
        .expect("requests")
        .into_iter()
        .find(|request| request.url.path() == path)
        .unwrap_or_else(|| panic!("no request was made to {path}"))
}
