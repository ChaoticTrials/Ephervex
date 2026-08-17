//! The shared HTTP client.
//!
//! One client for the whole process: connection pooling and TLS session reuse
//! are the difference between a pack import that takes thirty seconds and one
//! that takes three minutes, and a single construction point is the only way to
//! guarantee that every request Ephervex makes carries the same `User-Agent`
//! and the same transport rules.
//!
//! TLS is rustls. The workspace declares `reqwest` with
//! `default-features = false` and the `rustls` feature precisely so that
//! nothing in the dependency graph links OpenSSL — a launcher that has to build
//! and run on arbitrary Linux distributions cannot afford a system-OpenSSL
//! ABI dependency.

use std::time::Duration;

use ephervex_model::user_agent;
use reqwest::{Client, Url, redirect};
use tracing::warn;

use crate::allowlist::redacted;
use crate::error::DownloadError;

/// Transport-level settings for every outbound request.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Identifies the launcher to upstream services, including a project URL
    /// so they can contact us instead of silently blocking us.
    pub user_agent: String,

    /// How long to wait for a TCP connection and TLS handshake.
    pub connect_timeout: Duration,

    /// How long a *stalled* transfer is tolerated.
    ///
    /// Deliberately used instead of an overall request timeout: a managed JDK
    /// archive on a slow connection is legitimately a multi-minute download,
    /// and a wall-clock deadline would abort it. What must never happen is
    /// hanging forever on a connection that has stopped delivering bytes, and
    /// that is what a per-read timeout catches, producing an explicit error
    /// states, not indefinite spinners).
    pub read_timeout: Duration,

    /// Redirects are followed but bounded. CDNs redirect
    /// constantly, so refusing is not an option; following without a limit
    /// turns a redirect loop into a hang.
    pub max_redirects: usize,

    /// Escape hatch for the loopback HTTP servers used by the integration
    /// tests. Never enabled in production: `false` also switches reqwest into
    /// HTTPS-only mode, which additionally blocks an *https → http* downgrade
    /// part-way through a redirect chain.
    pub allow_plaintext_http: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            user_agent: user_agent(),
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(30),
            max_redirects: 5,
            allow_plaintext_http: false,
        }
    }
}

impl HttpConfig {
    /// Configuration for tests that talk to a loopback HTTP server, with
    /// timeouts short enough that a hung test fails rather than stalls CI.
    #[must_use]
    pub fn for_loopback_tests() -> Self {
        Self {
            connect_timeout: Duration::from_secs(2),
            read_timeout: Duration::from_secs(5),
            allow_plaintext_http: true,
            ..Self::default()
        }
    }
}

/// Builds the shared client.
pub fn build_client(config: &HttpConfig) -> Result<Client, DownloadError> {
    Client::builder()
        .user_agent(config.user_agent.clone())
        .https_only(!config.allow_plaintext_http)
        .redirect(redirect::Policy::limited(config.max_redirects))
        .connect_timeout(config.connect_timeout)
        .read_timeout(config.read_timeout)
        .build()
        .map_err(|source| DownloadError::ClientBuild { source })
}

/// Builds a client that will not follow a redirect off `origin`.
///
/// For the one situation the ordinary client cannot serve safely: a caller that
/// attaches a credential to every request it makes. `reqwest` strips the
/// headers it *knows* are sensitive — `Authorization`, `Cookie` — when a
/// redirect crosses hosts, but a provider's own credential header is a header
/// like any other, and following a `302` to somewhere else would hand the
/// secret to whoever the response named.
///
/// So a redirect that leaves the origin is stopped rather than followed. The
/// caller receives the `3xx` response itself and treats it as a failure, which
/// is the honest outcome: an API that redirects an authenticated request to
/// another host is not one this launcher can satisfy without leaking.
///
/// Redirects *within* the origin are still followed, bounded by
/// [`HttpConfig::max_redirects`], because those reach the host the credential
/// was issued for.
pub fn build_single_origin_client(
    config: &HttpConfig,
    origin: &Url,
) -> Result<Client, DownloadError> {
    let permitted = origin.origin();
    let max_redirects = config.max_redirects;
    let policy = redirect::Policy::custom(move |attempt| {
        if attempt.url().origin() != permitted {
            // The address, never its query string: a redirect target can carry
            // a signed token, and this line exists to explain a refusal.
            warn!(
                target = %redacted(attempt.url()),
                "refusing to follow an authenticated request off its own origin"
            );
            return attempt.stop();
        }
        if attempt.previous().len() >= max_redirects {
            return attempt.error(TooManyRedirects);
        }
        attempt.follow()
    });

    Client::builder()
        .user_agent(config.user_agent.clone())
        .https_only(!config.allow_plaintext_http)
        .redirect(policy)
        .connect_timeout(config.connect_timeout)
        .read_timeout(config.read_timeout)
        .build()
        .map_err(|source| DownloadError::ClientBuild { source })
}

/// What a redirect chain that never settles fails with.
///
/// `redirect::Policy::custom` reports a refusal through an arbitrary error
/// value rather than a status, so the bound needs a type to be expressed with.
#[derive(Debug)]
struct TooManyRedirects;

impl std::fmt::Display for TooManyRedirects {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("too many redirects")
    }
}

impl std::error::Error for TooManyRedirects {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_default_config_identifies_ephervex_and_refuses_plaintext() {
        let config = HttpConfig::default();
        assert!(config.user_agent.starts_with("Ephervex/"));
        assert!(
            config
                .user_agent
                .contains("github.com/ChaoticTrials/ephervex")
        );
        assert!(!config.allow_plaintext_http);
    }

    #[test]
    fn redirects_are_bounded_by_default() {
        assert_eq!(HttpConfig::default().max_redirects, 5);
    }

    #[test]
    fn the_client_builds_with_the_default_configuration() {
        assert!(build_client(&HttpConfig::default()).is_ok());
    }

    #[test]
    fn an_origin_scoped_client_builds_for_an_https_api_root() {
        let origin = Url::parse("https://api.example.test/v1/").expect("a url");
        assert!(build_single_origin_client(&HttpConfig::default(), &origin).is_ok());
    }
}
