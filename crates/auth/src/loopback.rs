//! The one-shot local HTTP listener that catches the browser redirect.
//!
//! The authorization code comes back as a query parameter on a redirect the
//! *browser* performs, so a desktop app has to be listening somewhere the
//! browser can reach. That somewhere is a loopback socket on an ephemeral port,
//! which is the pattern Microsoft documents for native apps and the reason
//! `http://localhost` is a permitted redirect URI for public clients despite
//! being plaintext: the bytes never leave the machine.
//!
//! # Why this is hand-written rather than an HTTP server crate
//!
//! It has to handle exactly one request shape - a `GET` with a query string -
//! and then stop. A server framework would bring routing, keep-alive, chunked
//! encoding, and a middleware stack to answer a single request, and every one
//! of those is attack surface bound to a socket that a hostile local process
//! can also connect to. What is here is a request-line parser with a hard byte
//! ceiling and a fixed response.
//!
//! # Five rules this module exists to enforce
//!
//! * **Bind loopback only.** `127.0.0.1` and `[::1]`, never `0.0.0.0`. The
//!   redirect carries an authorization code; nothing on the network may be able
//!   to send one.
//! * **Bound the read.** A connection that sends megabytes without a newline
//!   must not be able to exhaust memory, so the request line is capped.
//! * **Ignore requests that are not the redirect.** Browsers speculatively
//!   fetch `/favicon.ico`; answering it must not end the wait. A request whose
//!   `state` is not this login's is refused the same way: a stray local
//!   process must not be able to end a login that is still running.
//! * **Keep the deadline and cancellation in force.** Every accepted
//!   connection is served on its own task under a read timeout, so a peer that
//!   connects and stays silent can neither stop the wait from timing out or
//!   being canceled nor keep the real redirect from being accepted.
//! * **Never render the code.** The page shown to the user says "you can close
//!   this tab" and nothing else. The query string stays out of the response
//!   body, out of the page title, and out of every log line.

use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::error::{AuthError, AuthResult};
use crate::secret::SecretString;

/// The host used in the `redirect_uri` sent to Microsoft.
///
/// # Why `localhost` and not the IP literal
///
/// Microsoft's redirect-URI rules make this a one-way choice, and getting it
/// wrong fails at the very first sign-in with an unhelpful `invalid_request`.
/// From *Redirect URI (reply URL) best practices and limitations*, re-read
/// 2026-08-08:
///
/// * The port is ignored when matching **only** for `localhost` - *"This is
///   only true for localhost redirect URIs. In all other cases, the port
///   component is not ignored when matching redirect URIs."* An ephemeral port
///   is therefore only viable under `localhost`; under `127.0.0.1` every
///   listener would need a fixed, pre-registered port.
/// * *"To add a redirect URI that uses the `http` scheme with the `127.0.0.1`
///   loopback address, you must currently modify the `replyUrlsWithType`
///   attribute in the application manifest."* `http://localhost` goes in the
///   portal's own text box; the IP literal does not.
///
/// The Client ID is user-configurable, so anyone
/// running their own Azure registration has to be able to configure it. Making
/// that a JSON manifest edit plus a pinned port, rather than one checkbox in
/// the portal, would be a real cost paid by every such user.
///
/// # The IPv6 concern is real, and is fixed one layer down
///
/// `localhost` may resolve to `::1` before `127.0.0.1`, and a listener bound
/// only to the IPv4 loopback would then never see the redirect. That is a
/// *binding* problem, so [`LoopbackReceiver::bind`] answers on both loopback
/// addresses on the same port and it stops mattering which one the browser
/// picks. Microsoft never learns the difference: it only ever sees the string
/// `http://localhost:<port>`, which is just as well, since the same document
/// notes that `[::1]` is not supported as a registered redirect URI.
pub const REDIRECT_HOST: &str = "localhost";

/// Longest request line the listener will read before giving up on a
/// connection. A real redirect is a few hundred bytes.
const MAX_REQUEST_LINE: u64 = 8 * 1024;

/// How long to keep waiting for the browser before giving up.
///
/// Long enough for a password manager, a two-factor prompt, and someone
/// finding their phone; short enough that a forgotten login does not leave a
/// socket bound for the life of the process.
pub const DEFAULT_LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How long one accepted connection may take to deliver its request line
/// before it is dropped. A real browser sends the request immediately, but
/// speculative preconnections stay silent, so this only has to be generous
/// enough not to cut off a slow but genuine request.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

/// Most connections served at once. Bounds the sockets and tasks a local
/// process can pin; further connections wait in the kernel backlog until a
/// slot frees up or its read timeout fires.
const MAX_CONCURRENT_CONNECTIONS: usize = 64;

/// What the browser was redirected with.
#[derive(Debug)]
pub enum Redirect {
    /// The authorization code, which is a bearer credential until it is
    /// redeemed - hence [`SecretString`].
    Code(SecretString),
    /// Microsoft refused, or the user did.
    Error { code: String, description: String },
}

/// How many times to look for a port that is free on both loopback stacks.
///
/// Each attempt takes a fresh ephemeral port from the kernel, so a collision on
/// the IPv6 side is resolved by trying again rather than by scanning. Three is
/// generous: needing even a second attempt means something else is bound to the
/// exact port the kernel just handed out.
const BIND_ATTEMPTS: usize = 3;

/// A bound loopback socket waiting for one redirect.
///
/// Two listeners on one port, because `localhost` may resolve to either stack.
#[derive(Debug)]
pub struct LoopbackReceiver {
    /// `127.0.0.1`. Always present: a machine without an IPv4 loopback cannot
    /// run this flow at all.
    ipv4: TcpListener,
    /// `[::1]`, when the machine has IPv6 at all. `None` is not a degraded
    /// state - if IPv6 is disabled, `localhost` cannot resolve to `::1` either,
    /// so there is nothing left for it to catch.
    ipv6: Option<TcpListener>,
    redirect_uri: String,
}

impl LoopbackReceiver {
    /// Binds one ephemeral port on **both** loopback addresses.
    ///
    /// The browser resolves `localhost` however its own resolver prefers, and
    /// on a dual-stack machine that is frequently `::1`. Listening on only one
    /// stack turns that preference into an intermittent, machine-dependent
    /// hang, so both are bound and the resolver's choice stops mattering.
    pub async fn bind() -> AuthResult<Self> {
        let mut last: Option<std::io::Error> = None;

        for _ in 0..BIND_ATTEMPTS {
            let ipv4 = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
                .await
                .map_err(|source| AuthError::RedirectListener { source })?;
            let port = ipv4
                .local_addr()
                .map_err(|source| AuthError::RedirectListener { source })?
                .port();

            // `[::1]` is a v6-only address, so this never conflicts with the
            // IPv4 bind above; it can only conflict with a *different* process.
            match TcpListener::bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port)).await {
                Ok(ipv6) => return Ok(Self::new(ipv4, Some(ipv6), port)),
                // No IPv6 on this machine. Then `localhost` cannot resolve to
                // `::1`, and the IPv4 listener is the whole story.
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::AddrNotAvailable | ErrorKind::Unsupported
                    ) =>
                {
                    debug!(%error, "binding the sign-in listener to ipv4 only");
                    return Ok(Self::new(ipv4, None, port));
                }
                // Something else holds this port on the IPv6 stack. Take a
                // different one rather than leaving half the resolutions dead.
                Err(error) => {
                    debug!(port, %error, "port is taken on ipv6; retrying with another");
                    last = Some(error);
                }
            }
        }

        Err(AuthError::RedirectListener {
            source: last.unwrap_or_else(|| {
                std::io::Error::other("no loopback port was free on both address families")
            }),
        })
    }

    fn new(ipv4: TcpListener, ipv6: Option<TcpListener>, port: u16) -> Self {
        Self {
            ipv4,
            ipv6,
            // `localhost`, never the IP literal - see `REDIRECT_HOST`.
            redirect_uri: format!("http://{REDIRECT_HOST}:{port}"),
        }
    }

    /// Accepts on whichever loopback address the browser actually used.
    async fn accept(&self) -> std::io::Result<(TcpStream, SocketAddr)> {
        match &self.ipv6 {
            Some(ipv6) => tokio::select! {
                result = self.ipv4.accept() => result,
                result = ipv6.accept() => result,
            },
            None => self.ipv4.accept().await,
        }
    }

    /// The `redirect_uri` to send to Microsoft, and to send again when
    /// redeeming the code - the two must match exactly or the exchange fails.
    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Waits for the redirect, checking `state` before returning anything.
    ///
    /// A request whose `state` is not the one this login sent is a forged or
    /// stale redirect whose code is not ours to redeem. It is refused and the
    /// wait continues, still bound by `timeout` and `cancel`.
    pub async fn wait(
        self,
        expected_state: &str,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> AuthResult<Redirect> {
        self.wait_with(expected_state, timeout, CONNECTION_TIMEOUT, cancel)
            .await
    }

    async fn wait_with(
        self,
        expected_state: &str,
        timeout: Duration,
        connection_timeout: Duration,
        cancel: &CancellationToken,
    ) -> AuthResult<Redirect> {
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);

        let expected_state: Arc<str> = Arc::from(expected_state);
        // Dropping the set on any exit aborts connections still being served.
        let mut connections: JoinSet<Option<Redirect>> = JoinSet::new();

        loop {
            let has_capacity = connections.len() < MAX_CONCURRENT_CONNECTIONS;
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(AuthError::Canceled),
                () = &mut deadline => {
                    return Err(AuthError::TimedOut { seconds: timeout.as_secs() });
                }
                Some(finished) = connections.join_next(), if !connections.is_empty() => {
                    // `None` is a favicon probe, a refused state, a silent or
                    // malformed peer: the browser has not delivered the
                    // redirect yet, so keep waiting.
                    if let Ok(Some(redirect)) = finished {
                        return Ok(redirect);
                    }
                }
                accepted = self.accept(), if has_capacity => {
                    let (stream, peer) = accepted
                        .map_err(|source| AuthError::RedirectListener { source })?;
                    // Belt and braces: the socket is bound to loopback, so a
                    // non-loopback peer should be impossible. If the kernel
                    // ever disagrees, the authorization code is not going to it.
                    if !peer.ip().is_loopback() {
                        warn!("refusing a non-loopback connection to the sign-in listener");
                        continue;
                    }
                    let expected_state = Arc::clone(&expected_state);
                    connections.spawn(async move {
                        tokio::time::timeout(connection_timeout, handle(stream, &expected_state))
                            .await
                            .unwrap_or_else(|_| {
                                debug!("dropping a sign-in connection that stalled");
                                None
                            })
                    });
                }
            }
        }
    }
}

/// Reads one request, answers it, and reports the redirect if it was one.
///
/// Nothing a single connection does is fatal to the login, so a failure here
/// is `None` and the receiver keeps waiting.
async fn handle(stream: TcpStream, expected_state: &str) -> Option<Redirect> {
    let mut reader = BufReader::new(stream);
    let line = read_request_line(&mut reader).await?;

    let Some(target) = request_target(&line) else {
        debug!("ignoring a connection that did not send a GET request line");
        return None;
    };

    let redirect = match parse_redirect(&target, expected_state) {
        Ok(redirect) => redirect,
        Err(error) => {
            warn!(%error, "refusing a request to the sign-in listener");
            None
        }
    };
    // Every request gets an answer, including the ones that are not the
    // redirect: a browser left hanging shows an error page next to the tab
    // the user is meant to close.
    let page = match &redirect {
        Some(Redirect::Code(_)) => SUCCESS_PAGE,
        Some(Redirect::Error { .. }) => FAILURE_PAGE,
        None => IGNORED_PAGE,
    };
    let mut stream = reader.into_inner();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    // A browser that hung up before reading the page is not a reason to
    // fail a login that already succeeded.
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;

    redirect
}

/// Reads the request line, refusing one that never ends.
///
/// Hand-rolled rather than `read_line`, which is unbounded: a local process
/// that connects and streams bytes without a newline would otherwise be able to
/// grow this buffer until the launcher dies. `None` means the connection gave
/// nothing usable, which is a reason to keep waiting rather than to fail.
async fn read_request_line(reader: &mut BufReader<TcpStream>) -> Option<String> {
    let mut line = Vec::with_capacity(256);
    let mut byte = [0_u8; 1];
    loop {
        let read = match reader.read(&mut byte).await {
            Ok(read) => read,
            Err(error) => {
                debug!(%error, "reading a sign-in request failed");
                return None;
            }
        };
        if read == 0 {
            // Connection closed. Anything already read is only usable if the
            // client sent a complete line first, which it did not.
            return None;
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() as u64 >= MAX_REQUEST_LINE {
            warn!("dropping an over-long request to the sign-in listener");
            return None;
        }
    }
    // Lossy on purpose: a request line that is not UTF-8 is not the redirect,
    // and it will simply fail to parse as one.
    Some(String::from_utf8_lossy(&line).into_owned())
}

/// Extracts the request target from a request line, if it is a `GET`.
fn request_target(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    if !method.eq_ignore_ascii_case("GET") {
        return None;
    }
    Some(parts.next()?.to_owned())
}

/// Interprets `/?code=…&state=…` or `/?error=…`.
///
/// Returns `Ok(None)` for a request that is not the redirect at all - the
/// favicon probe every browser makes.
fn parse_redirect(target: &str, expected_state: &str) -> AuthResult<Option<Redirect>> {
    // A base is needed only because the request target is relative; the host is
    // never used and never reaches a request.
    let Ok(url) = Url::parse(&format!("http://{REDIRECT_HOST}{target}")) else {
        return Ok(None);
    };

    let mut code = None;
    let mut error = None;
    let mut description = String::new();
    let mut state = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            "error_description" => description = value.into_owned(),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }

    if code.is_none() && error.is_none() {
        return Ok(None);
    }

    // Checked before the code is looked at, and checked even on the error path:
    // a forged redirect must not be able to abort a login that is still running
    // legitimately in another tab.
    if state.as_deref() != Some(expected_state) {
        return Err(AuthError::StateMismatch);
    }

    if let Some(code) = code {
        return Ok(Some(Redirect::Code(SecretString::new(code))));
    }
    Ok(Some(Redirect::Error {
        code: error.unwrap_or_default(),
        description,
    }))
}

const SUCCESS_PAGE: &str = "<!doctype html><meta charset=utf-8>\
<title>Signed in</title><body style=\"font-family:system-ui;padding:3rem\">\
<h1>Signed in</h1><p>You can close this tab and return to Ephervex.</p>";

const FAILURE_PAGE: &str = "<!doctype html><meta charset=utf-8>\
<title>Sign-in failed</title><body style=\"font-family:system-ui;padding:3rem\">\
<h1>Sign-in was not completed</h1><p>You can close this tab and return to Ephervex.</p>";

const IGNORED_PAGE: &str = "<!doctype html><meta charset=utf-8>\
<title>Ephervex</title><body style=\"font-family:system-ui;padding:3rem\">\
<p>Waiting for the Microsoft sign-in to finish.</p>";

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// The port out of a `http://localhost:NNNNN` redirect URI.
    fn port_of(uri: &str) -> u16 {
        uri.rsplit(':')
            .next()
            .expect("a port")
            .parse()
            .expect("numeric port")
    }

    #[tokio::test]
    async fn the_redirect_uri_names_localhost_because_only_localhost_wildcards_the_port() {
        // This is not cosmetic and it is not interchangeable with the IP
        // literal. Microsoft ignores the port when matching a redirect URI
        // *only* for `localhost`; under `127.0.0.1` an ephemeral port would
        // have to be registered exactly, and an `http` loopback URI with the IP
        // literal cannot be added through the portal at all. Sending the
        // literal is what produced `invalid_request: The provided value for the
        // input parameter 'redirect_uri' is not valid` at the first real
        // sign-in, so it is pinned here.
        //
        // Change this and `docs/architecture.md`, and the manual
        // sign-in checklist all become wrong together.
        assert_eq!(REDIRECT_HOST, "localhost");

        let receiver = LoopbackReceiver::bind().await.expect("bind");
        let uri = receiver.redirect_uri().to_owned();
        assert!(uri.starts_with("http://localhost:"), "{uri}");
        assert!(
            !uri.contains("127.0.0.1") && !uri.contains("[::1]"),
            "the IP literal must not reach Microsoft: {uri}"
        );
        assert_ne!(
            port_of(&uri),
            0,
            "an ephemeral port must have been assigned"
        );
    }

    #[tokio::test]
    async fn the_listener_binds_loopback_only() {
        let receiver = LoopbackReceiver::bind().await.expect("bind");

        // The redirect carries an authorization code, so nothing off this
        // machine may be able to deliver one.
        let ipv4 = receiver.ipv4.local_addr().expect("addr");
        assert!(ipv4.ip().is_loopback(), "{ipv4}");
        if let Some(ipv6) = &receiver.ipv6 {
            let bound = ipv6.local_addr().expect("addr");
            assert!(bound.ip().is_loopback(), "{bound}");
            // Both stacks must answer on the *same* port, or the redirect URI
            // handed to Microsoft would only be right for one of them.
            assert_eq!(bound.port(), ipv4.port());
        }
    }

    #[tokio::test]
    async fn both_loopback_stacks_answer_on_the_bound_port() {
        // `localhost` resolves to `::1` first on plenty of machines. Binding
        // only IPv4 turns that into an intermittent hang that depends on the
        // user's resolver, which is exactly the kind of failure that is
        // impossible to diagnose from a bug report.
        let receiver = LoopbackReceiver::bind().await.expect("bind");
        let port = port_of(receiver.redirect_uri());
        let has_ipv6 = receiver.ipv6.is_some();

        let waiter = tokio::spawn(async move {
            receiver
                .wait(
                    "state-value",
                    Duration::from_secs(5),
                    &CancellationToken::new(),
                )
                .await
        });

        if has_ipv6 {
            // Reach it over IPv6 specifically, not over whatever `localhost`
            // happens to resolve to on the machine running the suite.
            let mut browser = TcpStream::connect((Ipv6Addr::LOCALHOST, port))
                .await
                .expect("the ipv6 loopback must answer");
            browser
                .write_all(b"GET /?code=V6-CODE&state=state-value HTTP/1.1\r\nHost: x\r\n\r\n")
                .await
                .expect("write");
        } else {
            let mut browser = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .expect("the ipv4 loopback must answer");
            browser
                .write_all(b"GET /?code=V6-CODE&state=state-value HTTP/1.1\r\nHost: x\r\n\r\n")
                .await
                .expect("write");
        }

        match waiter.await.expect("join").expect("a redirect") {
            Redirect::Code(code) => assert_eq!(code.expose(), "V6-CODE"),
            other @ Redirect::Error { .. } => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_ipv4_stack_answers_on_the_bound_port_too() {
        let receiver = LoopbackReceiver::bind().await.expect("bind");
        let port = port_of(receiver.redirect_uri());

        let waiter = tokio::spawn(async move {
            receiver
                .wait(
                    "state-value",
                    Duration::from_secs(5),
                    &CancellationToken::new(),
                )
                .await
        });

        let mut browser = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .expect("the ipv4 loopback must answer");
        browser
            .write_all(b"GET /?code=V4-CODE&state=state-value HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .expect("write");

        match waiter.await.expect("join").expect("a redirect") {
            Redirect::Code(code) => assert_eq!(code.expose(), "V4-CODE"),
            other @ Redirect::Error { .. } => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_browser_resolving_the_literal_hostname_reaches_the_listener() {
        // The end-to-end property the redirect URI actually depends on: connect
        // by *name*, the way the browser will, and let the machine's own
        // resolver pick the stack.
        let receiver = LoopbackReceiver::bind().await.expect("bind");
        let uri = receiver.redirect_uri().to_owned();
        let authority = uri.trim_start_matches("http://").to_owned();

        let waiter = tokio::spawn(async move {
            receiver
                .wait(
                    "state-value",
                    Duration::from_secs(5),
                    &CancellationToken::new(),
                )
                .await
        });

        let mut browser = TcpStream::connect(&authority)
            .await
            .expect("whatever `localhost` resolves to must answer");
        browser
            .write_all(b"GET /?code=BY-NAME&state=state-value HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .expect("write");

        match waiter.await.expect("join").expect("a redirect") {
            Redirect::Code(code) => assert_eq!(code.expose(), "BY-NAME"),
            other @ Redirect::Error { .. } => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn a_matching_redirect_yields_the_code() {
        let redirect = parse_redirect("/?code=M.C1_ABC&state=xyz", "xyz")
            .expect("valid")
            .expect("a redirect");
        match redirect {
            Redirect::Code(code) => assert_eq!(code.expose(), "M.C1_ABC"),
            other @ Redirect::Error { .. } => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn a_redirect_with_the_wrong_state_is_refused_before_the_code_is_read() {
        let error = parse_redirect("/?code=M.C1_ABC&state=forged", "xyz").expect_err("must refuse");
        assert!(matches!(error, AuthError::StateMismatch));

        //...and a redirect with no state at all is equally not ours.
        let error = parse_redirect("/?code=M.C1_ABC", "xyz").expect_err("must refuse");
        assert!(matches!(error, AuthError::StateMismatch));
    }

    #[test]
    fn an_error_redirect_is_also_state_checked() {
        // Otherwise anyone who can reach the port could cancel a login in
        // progress by sending `?error=access_denied`.
        let error =
            parse_redirect("/?error=access_denied&state=forged", "xyz").expect_err("must refuse");
        assert!(matches!(error, AuthError::StateMismatch));

        let redirect = parse_redirect(
            "/?error=access_denied&error_description=the+user+cancelled&state=xyz",
            "xyz",
        )
        .expect("valid")
        .expect("a redirect");
        match redirect {
            Redirect::Error { code, description } => {
                assert_eq!(code, "access_denied");
                assert_eq!(description, "the user cancelled");
            }
            other @ Redirect::Code(_) => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn a_favicon_probe_does_not_end_the_wait() {
        // Every browser makes this request, and treating it as the redirect
        // would abort the login the moment the tab opened.
        assert!(
            parse_redirect("/favicon.ico", "xyz")
                .expect("valid")
                .is_none()
        );
        assert!(parse_redirect("/", "xyz").expect("valid").is_none());
    }

    #[test]
    fn only_get_request_lines_are_considered() {
        assert_eq!(
            request_target("GET /?code=a&state=b HTTP/1.1\r\n").as_deref(),
            Some("/?code=a&state=b")
        );
        assert_eq!(request_target("get /x HTTP/1.1\r\n").as_deref(), Some("/x"));
        assert!(request_target("POST / HTTP/1.1\r\n").is_none());
        assert!(request_target("\r\n").is_none());
        assert!(request_target("GET\r\n").is_none());
    }

    #[test]
    fn the_page_shown_to_the_user_never_echoes_the_query_string() {
        for page in [SUCCESS_PAGE, FAILURE_PAGE, IGNORED_PAGE] {
            assert!(!page.contains("code"), "{page}");
            assert!(!page.contains("token"), "{page}");
        }
    }

    #[tokio::test]
    async fn a_browser_that_never_arrives_times_out_rather_than_hanging() {
        let receiver = LoopbackReceiver::bind().await.expect("bind");
        let error = receiver
            .wait("xyz", Duration::from_millis(50), &CancellationToken::new())
            .await
            .expect_err("must time out");
        assert!(matches!(error, AuthError::TimedOut { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn canceling_stops_the_wait_immediately() {
        let receiver = LoopbackReceiver::bind().await.expect("bind");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = receiver
            .wait("xyz", Duration::from_secs(30), &cancel)
            .await
            .expect_err("must stop");
        assert!(error.is_cancellation(), "{error:?}");
    }

    #[tokio::test]
    async fn a_real_browser_redirect_is_accepted_and_the_tab_gets_a_page() {
        let receiver = LoopbackReceiver::bind().await.expect("bind");
        let uri = receiver.redirect_uri().to_owned();

        let waiter = tokio::spawn(async move {
            receiver
                .wait(
                    "state-value",
                    Duration::from_secs(5),
                    &CancellationToken::new(),
                )
                .await
        });

        // Stand in for the browser: the favicon probe first, then the redirect.
        let base = uri.trim_start_matches("http://").to_owned();
        let mut probe = TcpStream::connect(&base).await.expect("connect");
        probe
            .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .expect("write");
        drop(probe);

        let mut browser = TcpStream::connect(&base).await.expect("connect");
        browser
            .write_all(b"GET /?code=THE-CODE&state=state-value HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .expect("write");

        let mut page = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut browser, &mut page)
            .await
            .expect("read the page");
        let page = String::from_utf8_lossy(&page);
        assert!(page.contains("200 OK"), "{page}");
        assert!(page.contains("Signed in"), "{page}");
        assert!(
            !page.contains("THE-CODE"),
            "the page must not echo the code"
        );

        match waiter.await.expect("join").expect("a redirect") {
            Redirect::Code(code) => assert_eq!(code.expose(), "THE-CODE"),
            other @ Redirect::Error { .. } => panic!("unexpected: {other:?}"),
        }
    }

    /// Connects and sends one byte of a request line, then stays silent.
    async fn idle_peer(uri: &str) -> TcpStream {
        let mut peer = TcpStream::connect(uri.trim_start_matches("http://"))
            .await
            .expect("connect");
        peer.write_all(b"G").await.expect("write");
        peer
    }

    #[tokio::test]
    async fn an_idle_peer_cannot_keep_the_wait_from_timing_out() {
        let receiver = LoopbackReceiver::bind().await.expect("bind");
        let _peer = idle_peer(receiver.redirect_uri()).await;

        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            receiver.wait("xyz", Duration::from_millis(100), &CancellationToken::new()),
        )
        .await
        .expect("the wait must end at its own deadline");
        assert!(
            matches!(outcome, Err(AuthError::TimedOut { .. })),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn an_idle_peer_cannot_keep_cancellation_from_ending_the_wait() {
        let receiver = LoopbackReceiver::bind().await.expect("bind");
        let _peer = idle_peer(receiver.redirect_uri()).await;
        let cancel = CancellationToken::new();

        let canceler = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            canceler.cancel();
        });

        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            receiver.wait("xyz", Duration::from_secs(60), &cancel),
        )
        .await
        .expect("cancellation must end the wait");
        assert!(
            matches!(&outcome, Err(error) if error.is_cancellation()),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn an_idle_peer_does_not_stop_the_real_redirect_from_being_served() {
        let receiver = LoopbackReceiver::bind().await.expect("bind");
        let uri = receiver.redirect_uri().to_owned();
        let _idle = idle_peer(&uri).await;

        let waiter = tokio::spawn(async move {
            receiver
                .wait(
                    "state-value",
                    Duration::from_secs(5),
                    &CancellationToken::new(),
                )
                .await
        });

        let mut browser = TcpStream::connect(uri.trim_start_matches("http://"))
            .await
            .expect("connect");
        browser
            .write_all(b"GET /?code=THE-CODE&state=state-value HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .expect("write");

        match waiter.await.expect("join").expect("a redirect") {
            Redirect::Code(code) => assert_eq!(code.expose(), "THE-CODE"),
            other @ Redirect::Error { .. } => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_peer_that_stays_silent_is_disconnected_after_the_connection_timeout() {
        let receiver = LoopbackReceiver::bind().await.expect("bind");
        let mut peer = idle_peer(receiver.redirect_uri()).await;

        let waiter = tokio::spawn(async move {
            receiver
                .wait_with(
                    "xyz",
                    Duration::from_secs(5),
                    Duration::from_millis(100),
                    &CancellationToken::new(),
                )
                .await
        });

        // The receiver ends the connection without sending a page: the peer
        // never produced a request line, so there is nothing to answer.
        let mut rest = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(3),
            tokio::io::AsyncReadExt::read_to_end(&mut peer, &mut rest),
        )
        .await
        .expect("the receiver must hang up on a silent peer")
        .ok();
        assert!(rest.is_empty(), "{rest:?}");
        waiter.abort();
    }

    #[tokio::test]
    async fn a_wrong_state_request_is_refused_and_the_wait_still_accepts_the_real_redirect() {
        let receiver = LoopbackReceiver::bind().await.expect("bind");
        let authority = receiver
            .redirect_uri()
            .trim_start_matches("http://")
            .to_owned();

        let waiter = tokio::spawn(async move {
            receiver
                .wait(
                    "state-value",
                    Duration::from_secs(5),
                    &CancellationToken::new(),
                )
                .await
        });

        let mut stray = TcpStream::connect(&authority).await.expect("connect");
        stray
            .write_all(b"GET /?code=STOLEN&state=forged HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .expect("write");
        let mut page = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stray, &mut page)
            .await
            .expect("read the page");
        let page = String::from_utf8_lossy(&page);
        assert!(!page.contains("Signed in"), "{page}");
        assert!(!page.contains("STOLEN"), "{page}");
        assert!(
            !waiter.is_finished(),
            "a forged state must not end the wait"
        );

        let mut browser = TcpStream::connect(&authority).await.expect("connect");
        browser
            .write_all(b"GET /?code=THE-CODE&state=state-value HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .expect("write");

        match waiter.await.expect("join").expect("a redirect") {
            Redirect::Code(code) => assert_eq!(code.expose(), "THE-CODE"),
            other @ Redirect::Error { .. } => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_forged_error_redirect_does_not_cancel_a_login_in_progress() {
        let receiver = LoopbackReceiver::bind().await.expect("bind");
        let uri = receiver.redirect_uri().to_owned();

        let outcome = tokio::spawn(async move {
            receiver
                .wait("xyz", Duration::from_millis(500), &CancellationToken::new())
                .await
        });

        let mut stray = TcpStream::connect(uri.trim_start_matches("http://"))
            .await
            .expect("connect");
        stray
            .write_all(b"GET /?error=access_denied&state=forged HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .expect("write");

        let error = outcome.await.expect("join").expect_err("must time out");
        assert!(matches!(error, AuthError::TimedOut { .. }), "{error:?}");
    }
}
