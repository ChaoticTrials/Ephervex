//! Proof that the CurseForge API key never reaches `tracing` (ADR 0004,
//! ADR 0022).
//!
//! # Why this is its own test binary
//!
//! `tracing` caches each callsite's *interest* the first time it is evaluated.
//! In a binary where some tests run before a subscriber exists, those callsites
//! can be recorded as permanently uninteresting, and a thread-local subscriber
//! installed later then captures nothing — a capture that silently records zero
//! events would make this test pass while proving nothing at all.
//!
//! Cargo gives each file under `tests/` its own process, so this one installs a
//! *global* subscriber before anything else happens.
//!
//! # Why the assertion is on captured output rather than on `Debug`
//!
//! The ways a credential escapes are indirect: a `debug!` that names a request,
//! a `#[instrument]` deriving a span field from an argument, an error variant
//! that captured a header on its way past. Only reading what `tracing` really
//! emitted catches those.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use ephervex_download::HttpConfig;
use ephervex_mod_platform::{ConfiguredApiKey, CurseForgeApi, MINECRAFT_GAME_ID};
use ephervex_model::SecretString;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Spelled distinctively so a substring search cannot miss it and cannot match
/// something innocent. Obviously not a credential.
const FAKE_KEY: &str = "FAKE-CURSEFORGE-API-KEY-DO-NOT-USE";

/// A `MakeWriter` that keeps everything `tracing` emitted.
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

fn api(base: &str) -> CurseForgeApi {
    CurseForgeApi::with_base_url(
        Arc::new(ConfiguredApiKey::new(SecretString::new(FAKE_KEY))),
        &HttpConfig::for_loopback_tests(),
        base,
    )
    .expect("a client builds")
}

#[tokio::test]
async fn nothing_a_curseforge_request_logs_contains_the_api_key() {
    let captured = &*CAPTURE;

    let server = MockServer::start().await;
    // Every documented outcome in one run: a success, a refusal that blames the
    // key, and a body that cannot be read. All three log, and none may log the
    // credential.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"data":{"id":432,"name":"Minecraft","slug":"minecraft"}}"#,
            "application/json",
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_raw("nope", "text/plain"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("not json", "application/json"))
        .mount(&server)
        .await;

    let api = api(&format!("{}/v1", server.uri()));
    let cancel = CancellationToken::new();
    let outcomes = [
        api.game(MINECRAFT_GAME_ID, &cancel).await.err(),
        api.game(MINECRAFT_GAME_ID, &cancel).await.err(),
        api.game(MINECRAFT_GAME_ID, &cancel).await.err(),
    ];

    // The errors themselves travel into logs through `{error:?}` and `{error}`,
    // so they are checked the same way the log is.
    for outcome in outcomes.iter().flatten() {
        assert!(!format!("{outcome:?}").contains(FAKE_KEY), "{outcome:?}");
        assert!(!outcome.to_string().contains(FAKE_KEY), "{outcome}");
    }

    let log = captured.contents();
    assert!(
        log.contains("curseforge api response"),
        "the capture recorded nothing, so it proves nothing:\n{log}"
    );
    assert!(!log.contains(FAKE_KEY), "the api key reached a log:\n{log}");
    // The header name may appear in this crate's own source; its value must not
    // appear anywhere, and neither must a rendering of the request's headers.
    assert!(!log.to_ascii_lowercase().contains("x-api-key:"), "{log}");
}
