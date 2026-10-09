//! Proof that no credential reaches `tracing` (ADR 0004).
//!
//! # Why this is its own test binary
//!
//! `tracing` caches each callsite's *interest* the first time it is evaluated.
//! In a binary where some tests run before a subscriber exists, those callsites
//! can be recorded as permanently uninteresting, and a thread-local subscriber
//! installed later then captures nothing - a capture that silently records zero
//! events would make this test pass while proving nothing at all.
//!
//! Cargo gives each file under `tests/` its own process, so this one installs a
//! *global* subscriber before anything else happens and every event in the
//! process lands in the buffer.
//!
//! # Why the assertion is on captured output rather than on `Debug`
//!
//! Asserting `format!("{session:?}")` is free of tokens is worth doing and is
//! not sufficient. The ways a credential actually escapes are indirect:
//! `#[instrument]` deriving a span field from a function argument, a `debug!`
//! that names a request body, an error variant that captured a header on its
//! way past. All three produce output nobody wrote by hand, and only reading
//! what `tracing` really emitted catches them.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use ephervex_auth::{
    AccountProvider, AuthEvent, AuthObserver, CredentialKey, CredentialStore, Endpoints, LoginFlow,
    LoginRequest, MemoryStore, MicrosoftAccountProvider, SecretString,
};
use ephervex_download::HttpConfig;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT_ID: &str = "767ea465-0bba-443b-a06b-831c71777b90";
const ACCOUNT: &str = "069a79f4-44e9-4726-a5be-fca90e38aaf5";

const TOKEN: &str = include_str!("../../../fixtures/microsoft/microsoft-token.json");
const DEVICE_CODE: &str = include_str!("../../../fixtures/microsoft/microsoft-device-code.json");
const XBOX: &str = include_str!("../../../fixtures/microsoft/xbox-authenticate.json");
const XSTS: &str = include_str!("../../../fixtures/microsoft/xsts-authorize.json");
const XSTS_DENIED: &str = include_str!("../../../fixtures/microsoft/xsts-denied.json");
const MC_LOGIN: &str = include_str!("../../../fixtures/microsoft/minecraft-login.json");
const ENTITLEMENTS: &str = include_str!("../../../fixtures/microsoft/minecraft-entitlements.json");
const PROFILE: &str = include_str!("../../../fixtures/microsoft/minecraft-profile.json");

/// Every credential value the fixtures carry, plus the authorization code the
/// browser stand-in delivers.
///
/// They are spelled distinctively so a substring search cannot miss one and
/// cannot match something innocent.
const FIXTURE_SECRETS: &[&str] = &[
    "FAKE-MICROSOFT-ACCESS-TOKEN",
    "FAKE-MICROSOFT-REFRESH-TOKEN",
    "FAKE-XBOX-USER-TOKEN",
    "FAKE-XSTS-TOKEN",
    "FAKE-MINECRAFT-ACCESS-TOKEN",
    "FAKE-DEVICE-CODE",
    "THE-AUTH-CODE",
];

// -------------------------------------------------------------- the capture

/// A `MakeWriter` that keeps everything `tracing` emitted, for the life of the
/// process.
#[derive(Clone, Default)]
struct CapturedLog(Arc<Mutex<Vec<u8>>>);

impl CapturedLog {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

impl std::io::Write for CapturedLog {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLog {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The process-wide capture, installed on first touch.
///
/// `LazyLock` rather than a `#[ctor]` or a per-test call: `set_global_default`
/// may only be called once, and every test in this file needs it to have
/// happened already.
static CAPTURE: LazyLock<CapturedLog> = LazyLock::new(|| {
    let captured = CapturedLog::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(captured.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this binary installs the only subscriber");
    captured
});

/// Fails if any fixture credential appears in `text`.
fn assert_no_secret(text: &str, what: &str) {
    for secret in FIXTURE_SECRETS {
        assert!(
            !text.contains(secret),
            "{secret} appeared in {what}:\n{text}"
        );
    }
}

// ---------------------------------------------------------------- scaffolding

async fn mount_full_chain(server: &MockServer) {
    for (verb, route, status, body) in [
        ("POST", "/consumers/oauth2/v2.0/token", 200, TOKEN),
        (
            "POST",
            "/consumers/oauth2/v2.0/devicecode",
            200,
            DEVICE_CODE,
        ),
        ("POST", "/user/authenticate", 200, XBOX),
        ("POST", "/xsts/authorize", 200, XSTS),
        ("POST", "/authentication/login_with_xbox", 200, MC_LOGIN),
        ("GET", "/entitlements/mcstore", 200, ENTITLEMENTS),
        ("GET", "/minecraft/profile", 200, PROFILE),
    ] {
        Mock::given(method(verb))
            .and(path(route))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(server)
            .await;
    }
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

/// Plays the part of the browser: performs the redirect Microsoft would.
async fn drive_browser_login(
    provider: &MicrosoftAccountProvider,
) -> Result<ephervex_auth::Login, ephervex_auth::AuthError> {
    let (observer, mut events) = AuthObserver::channel();
    let browser = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let AuthEvent::OpenBrowser { url } = event {
                let parsed = reqwest::Url::parse(&url).expect("an authorization url");
                let query: std::collections::HashMap<_, _> = parsed
                    .query_pairs()
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect();
                let target = format!(
                    "{}/?code=THE-AUTH-CODE&state={}",
                    query["redirect_uri"], query["state"]
                );
                let _ = reqwest::Client::new().get(&target).send().await;
                return;
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
    drop(observer);
    let _ = browser.await;
    session
}

// --------------------------------------------------------------------- tests

#[tokio::test]
async fn the_capture_is_really_capturing() {
    // Without this, every other test in the file would pass against an empty
    // buffer and prove nothing. `http::send` emits this on every leg.
    let capture = &*CAPTURE;
    let server = MockServer::start().await;
    mount_full_chain(&server).await;

    drive_browser_login(&provider(&server, Arc::new(MemoryStore::new())))
        .await
        .expect("chain completes");

    let log = capture.contents();
    assert!(
        log.contains("authentication response"),
        "the subscriber caught nothing from the chain:\n{log}"
    );
    assert!(
        log.contains("authentication chain completed"),
        "the chain's own completion event is missing:\n{log}"
    );
}

#[tokio::test]
async fn a_successful_browser_login_emits_no_credential_at_trace_level() {
    let capture = &*CAPTURE;
    let server = MockServer::start().await;
    mount_full_chain(&server).await;

    let session = drive_browser_login(&provider(&server, Arc::new(MemoryStore::new())))
        .await
        .expect("chain completes");

    assert_no_secret(&capture.contents(), "the trace-level log");
    // The value a frontend is most likely to render, checked separately: a
    // `Debug` that leaks is a leak even if nothing logged it today.
    assert_no_secret(&format!("{session:?}"), "the login's Debug output");
}

#[tokio::test]
async fn a_device_code_login_emits_neither_the_device_code_nor_the_tokens() {
    // The device code is a bearer credential for the poll, and it is the one
    // most easily mistaken for display material because a *different* code -
    // the user code - is meant to be shown.
    let capture = &*CAPTURE;
    let server = MockServer::start().await;
    mount_full_chain(&server).await;

    let session = provider(&server, Arc::new(MemoryStore::new()))
        .login(
            &LoginRequest {
                flow: LoginFlow::DeviceCode,
                timeout: Duration::from_secs(10),
            },
            &AuthObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect("chain completes");

    assert_no_secret(&capture.contents(), "the trace-level log");
    assert_no_secret(&format!("{session:?}"), "the login's Debug output");
}

#[tokio::test]
async fn a_refresh_emits_neither_the_old_nor_the_rotated_credential() {
    let capture = &*CAPTURE;
    let server = MockServer::start().await;
    mount_full_chain(&server).await;

    let store = Arc::new(MemoryStore::new());
    let account = Uuid::parse_str(ACCOUNT).expect("uuid");
    store
        .store(
            &CredentialKey::MicrosoftRefreshToken(account),
            &SecretString::new("FAKE-MICROSOFT-REFRESH-TOKEN"),
        )
        .expect("seed a previous login");

    provider(&server, store)
        .resume(account, &AuthObserver::silent(), &CancellationToken::new())
        .await
        .expect("resume succeeds");

    assert_no_secret(&capture.contents(), "the trace-level log");
}

#[tokio::test]
async fn a_failed_chain_emits_no_credential_and_returns_none_in_its_error() {
    // Errors reach logs by construction - `{error:?}`, `#[instrument]`, and the
    // CLI walking `error.chain()` - so a variant that captured a token would be
    // a leak with extra steps.
    let capture = &*CAPTURE;
    let server = MockServer::start().await;
    for (verb, route, status, body) in [
        ("POST", "/consumers/oauth2/v2.0/token", 200, TOKEN),
        ("POST", "/user/authenticate", 200, XBOX),
        ("POST", "/xsts/authorize", 401, XSTS_DENIED),
    ] {
        Mock::given(method(verb))
            .and(path(route))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
    }

    let error = drive_browser_login(&provider(&server, Arc::new(MemoryStore::new())))
        .await
        .expect_err("xsts refused");

    assert_no_secret(&format!("{error:?} {error}"), "the returned error");
    assert_no_secret(&capture.contents(), "the trace-level log");
}
