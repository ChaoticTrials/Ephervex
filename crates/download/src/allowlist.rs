//! The download-domain allowlist and URL policy.
//!
//! A `.mrpack` is untrusted input taken straight out of a bug report: its
//! `files[].downloads` array is attacker-controlled text. Fetching whatever it
//! names would turn the launcher into a confused deputy - an SSRF probe against
//! the reporter's own network, or a `file://` read of their home directory.
//!
//! Modrinth's pack format therefore asks implementers to define an explicit set
//! of permitted download domains, and this module is that set. It is
//! deliberately a standalone, auditable API with no dependency on the download
//! manager: Modrinth pack import's `.mrpack` importer is the consumer, and the list is
//! expected to be reviewed and updated over time rather than quietly grown.

use std::collections::BTreeSet;

use reqwest::Url;

/// Hosts Modrinth's own pack validation accepts for `.mrpack` file downloads.
///
/// Source: the Modrinth Modpack Format (`.mrpack`) specification, which states
/// that a pack's `downloads` entries must point at Modrinth's CDN or at one of
/// a small number of source-hosting domains, and recommends third-party
/// implementers enforce the same restriction.
///
/// - <https://support.modrinth.com/en/articles/8802351-modrinth-modpack-format-mrpack>
///
/// Audit note: keep this list minimal and exact-match. Every addition widens
/// the set of hosts an untrusted bug-report archive can make the launcher talk
/// to, so an entry belongs here only if Modrinth itself accepts it, or if a
/// deliberate product decision (recorded in an ADR) says otherwise. Wildcards
/// are intentionally not supported - `*.example.com` is a subdomain-takeover
/// surface, and none of the entries below need one.
pub const MODRINTH_PACK_HOSTS: &[&str] = &[
    "cdn.modrinth.com",
    "github.com",
    "raw.githubusercontent.com",
    "gitlab.com",
];

/// Hosts CurseForge delivers project files from.
///
/// A CurseForge export manifest names no download address: the address comes
/// back from the third-party API as a `downloadUrl`. That is provider data
/// rather than pack data, but it is still remote input that decides which
/// machine the launcher connects to - and, because a CurseForge artifact
/// request is the one download that may carry a credential, which machine could
/// be handed one. So it is checked against this list like any other.
///
/// Audit note: every entry is a CurseForge-operated delivery host observed in
/// the API's own `downloadUrl` answers. Keep the list minimal and exact-match;
/// wildcards are intentionally unsupported here for the same subdomain-takeover
/// reason they are unsupported for [`MODRINTH_PACK_HOSTS`]. An address the API
/// returns that is not on this list is refused rather than followed, and adding
/// a host widens where a credential may be sent.
pub const CURSEFORGE_ARTIFACT_HOSTS: &[&str] = &[
    "edge.forgecdn.net",
    "media.forgecdn.net",
    "mediafilez.forgecdn.net",
];

/// Why a URL from imported metadata was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum UrlPolicyError {
    /// Not a URL at all.
    #[error("`{url}` is not a well-formed url")]
    Malformed { url: String },

    /// `file://`, `http://`, `data:`, `ftp://`, and friends. Rejecting these is
    /// the whole point: a pack must not be able to name a local path.
    #[error("scheme `{scheme}` is not permitted for downloads")]
    SchemeNotPermitted { url: String, scheme: String },

    /// A URL with no host, e.g. `https:///foo` or a relative path.
    #[error("`{url}` has no host")]
    MissingHost { url: String },

    /// The host is well-formed but not on the allowlist.
    #[error("host `{host}` is not on the download allowlist")]
    HostNotAllowed { url: String, host: String },

    /// `https://allowed.example@evil.example/` parses with host `evil.example`,
    /// but userinfo in a URL is a classic way to make a human misread the
    /// destination, and no legitimate CDN needs it.
    #[error("`{url}` embeds credentials")]
    CredentialsInUrl { url: String },

    /// A non-default port on an allowlisted host is a redirect to somewhere
    /// else on that machine, which the allowlist was not asked to vouch for.
    #[error("port {port} is not permitted for `{url}`")]
    PortNotPermitted { url: String, port: u16 },
}

/// The one scheme artifacts may be fetched over.
const HTTPS: &str = "https";

/// Strips anything that could be a secret from a URL before it reaches a log
/// line or an error payload.
///
/// Query strings on CDN URLs routinely carry signed tokens, and userinfo
/// carries credentials by definition. Neither belongs in a log file that a user
/// might paste into a bug report.
#[must_use]
pub fn redacted(url: &Url) -> String {
    let mut cleaned = url.clone();
    cleaned.set_query(None);
    cleaned.set_fragment(None);
    // Both setters only fail for cannot-be-a-base URLs, which have no userinfo
    // to strip in the first place.
    let _ = cleaned.set_username("");
    let _ = cleaned.set_password(None);
    cleaned.to_string()
}

/// Enforces the transport rules that apply to *every* download, allowlisted or
/// not: HTTPS only, and no credentials smuggled into the authority.
///
/// `allow_plaintext_http` exists solely so integration tests can point the
/// download manager at a loopback HTTP server. It is `false` everywhere in
/// production; see [`crate::client::HttpConfig`].
pub fn check_transport(url: &Url, allow_plaintext_http: bool) -> Result<(), UrlPolicyError> {
    let display = redacted(url);
    let scheme = url.scheme();
    let permitted = scheme == HTTPS || (allow_plaintext_http && scheme == "http");
    if !permitted {
        return Err(UrlPolicyError::SchemeNotPermitted {
            url: display,
            scheme: scheme.to_owned(),
        });
    }
    if url.host_str().is_none() {
        return Err(UrlPolicyError::MissingHost { url: display });
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlPolicyError::CredentialsInUrl { url: display });
    }
    Ok(())
}

/// An explicit set of hosts a download URL may name.
///
/// Matching is exact on the host, after ASCII lowercasing. The `url` crate has
/// already punycoded any internationalized host by the time it gets here, so a
/// Unicode homograph cannot slip past a comparison against an ASCII entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainAllowlist {
    hosts: BTreeSet<String>,
    allow_plaintext_http: bool,
    allow_non_default_ports: bool,
}

impl DomainAllowlist {
    /// The list Ephervex applies to `.mrpack` downloads: see
    /// [`MODRINTH_PACK_HOSTS`] for the source and the audit rules.
    #[must_use]
    pub fn modrinth_pack_defaults() -> Self {
        Self::new(MODRINTH_PACK_HOSTS.iter().copied())
    }

    /// The list Ephervex applies to CurseForge artifact downloads: see
    /// [`CURSEFORGE_ARTIFACT_HOSTS`] for the source and the audit rules.
    #[must_use]
    pub fn curseforge_artifact_defaults() -> Self {
        Self::new(CURSEFORGE_ARTIFACT_HOSTS.iter().copied())
    }

    /// An allowlist that permits nothing. Every URL checked against it is
    /// refused, which is the correct default for a new import source.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Builds an allowlist from an explicit host list.
    #[must_use]
    pub fn new<I, S>(hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            hosts: hosts
                .into_iter()
                .map(|host| host.as_ref().trim().to_ascii_lowercase())
                .filter(|host| !host.is_empty())
                .collect(),
            allow_plaintext_http: false,
            allow_non_default_ports: false,
        }
    }

    /// Builds an allowlist for an HTTP loopback integration-test server.
    ///
    /// This is the allowlist counterpart to
    /// [`crate::client::HttpConfig::for_loopback_tests`]. Production
    /// constructors always require HTTPS.
    #[must_use]
    pub fn for_loopback_tests(host: &str) -> Self {
        Self {
            hosts: [host.trim().to_ascii_lowercase()]
                .into_iter()
                .filter(|host| !host.is_empty())
                .collect(),
            allow_plaintext_http: true,
            allow_non_default_ports: true,
        }
    }

    /// Adds one host. Kept explicit and chainable so a caller that widens the
    /// list has to say so at the call site.
    #[must_use]
    pub fn allowing(mut self, host: &str) -> Self {
        let host = host.trim().to_ascii_lowercase();
        if !host.is_empty() {
            self.hosts.insert(host);
        }
        self
    }

    /// The permitted hosts, sorted - suitable for rendering in a settings
    /// screen or a `--json` dump so a user can see what the launcher will talk
    /// to.
    pub fn hosts(&self) -> impl ExactSizeIterator<Item = &str> {
        self.hosts.iter().map(String::as_str)
    }

    /// Whether a bare host string is permitted.
    #[must_use]
    pub fn permits_host(&self, host: &str) -> bool {
        self.hosts.contains(&host.trim().to_ascii_lowercase())
    }

    /// Parses and fully validates a URL taken from pack metadata.
    ///
    /// This is the function importers should call. It returns the parsed URL so
    /// a caller cannot accidentally validate one string and then fetch another.
    pub fn validate(&self, raw: &str) -> Result<Url, UrlPolicyError> {
        let url = Url::parse(raw.trim()).map_err(|_| UrlPolicyError::Malformed {
            url: raw.trim().to_owned(),
        })?;
        self.validate_url(&url)?;
        Ok(url)
    }

    /// Validates an already-parsed URL.
    pub fn validate_url(&self, url: &Url) -> Result<(), UrlPolicyError> {
        check_transport(url, self.allow_plaintext_http)?;

        let display = redacted(url);
        let Some(host) = url.host_str() else {
            return Err(UrlPolicyError::MissingHost { url: display });
        };
        if let Some(port) = url.port().filter(|_| !self.allow_non_default_ports) {
            // `Url::port` is `None` when the port is the scheme default, so
            // anything here is an explicit non-443 port.
            return Err(UrlPolicyError::PortNotPermitted { url: display, port });
        }
        if !self.permits_host(host) {
            return Err(UrlPolicyError::HostNotAllowed {
                url: display,
                host: host.to_ascii_lowercase(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn modrinth() -> DomainAllowlist {
        DomainAllowlist::modrinth_pack_defaults()
    }

    #[test]
    fn the_default_allowlist_is_exactly_the_documented_modrinth_hosts() {
        let list = modrinth();
        let hosts: Vec<&str> = list.hosts().collect();
        assert_eq!(
            hosts,
            vec![
                "cdn.modrinth.com",
                "github.com",
                "gitlab.com",
                "raw.githubusercontent.com",
            ]
        );
    }

    #[test]
    fn the_curseforge_artifact_list_is_exactly_the_delivery_hosts_and_nothing_else() {
        let list = DomainAllowlist::curseforge_artifact_defaults();
        let hosts: Vec<&str> = list.hosts().collect();
        assert_eq!(
            hosts,
            vec![
                "edge.forgecdn.net",
                "media.forgecdn.net",
                "mediafilez.forgecdn.net",
            ]
        );
        // The API origin is not a delivery host, and a delivery host is not the
        // API origin. Neither vouches for the other.
        assert!(!list.permits_host("api.curseforge.com"));
        assert!(!list.permits_host("www.curseforge.com"));
        assert!(!list.permits_host("evil.forgecdn.net.example"));
        assert!(!list.permits_host("cdn.modrinth.com"));
    }

    #[test]
    fn an_allowlisted_https_url_is_accepted() {
        let url = modrinth()
            .validate("https://cdn.modrinth.com/data/AABBCCDD/versions/1/mod.jar")
            .expect("cdn.modrinth.com is allowlisted");
        assert_eq!(url.host_str(), Some("cdn.modrinth.com"));
    }

    #[test]
    fn a_file_url_is_refused() {
        let error = modrinth().validate("file:///etc/passwd").unwrap_err();
        assert!(matches!(
            error,
            UrlPolicyError::SchemeNotPermitted { ref scheme,.. } if scheme == "file"
        ));
    }

    #[test]
    fn plaintext_http_is_refused_even_for_an_allowlisted_host() {
        let error = modrinth()
            .validate("http://cdn.modrinth.com/data/x/mod.jar")
            .unwrap_err();
        assert!(matches!(
            error,
            UrlPolicyError::SchemeNotPermitted { ref scheme,.. } if scheme == "http"
        ));
    }

    #[test]
    fn a_relative_path_is_refused_rather_than_resolved() {
        let error = modrinth().validate("../../etc/shadow").unwrap_err();
        assert!(matches!(error, UrlPolicyError::Malformed { .. }));
    }

    #[test]
    fn an_unlisted_host_is_refused() {
        let error = modrinth()
            .validate("https://evil.example/mod.jar")
            .unwrap_err();
        assert!(matches!(
            error,
            UrlPolicyError::HostNotAllowed { ref host,.. } if host == "evil.example"
        ));
    }

    #[test]
    fn userinfo_does_not_make_an_unlisted_host_look_allowlisted() {
        let error = modrinth()
            .validate("https://cdn.modrinth.com@evil.example/mod.jar")
            .unwrap_err();
        assert!(matches!(error, UrlPolicyError::CredentialsInUrl { .. }));
    }

    #[test]
    fn an_explicit_port_on_an_allowlisted_host_is_refused() {
        let error = modrinth()
            .validate("https://github.com:8443/owner/repo/releases/mod.jar")
            .unwrap_err();
        assert!(matches!(
            error,
            UrlPolicyError::PortNotPermitted { port: 8443, .. }
        ));
    }

    #[test]
    fn host_matching_is_case_insensitive_but_not_suffix_based() {
        let list = modrinth();
        assert!(list.permits_host("CDN.Modrinth.COM"));
        // A subdomain-takeover surface: `evil.github.com` must not inherit
        // `github.com`'s entry.
        assert!(!list.permits_host("evil.github.com"));
        assert!(!list.permits_host("notgithub.com"));
    }

    #[test]
    fn an_empty_allowlist_permits_nothing() {
        let error = DomainAllowlist::empty()
            .validate("https://cdn.modrinth.com/mod.jar")
            .unwrap_err();
        assert!(matches!(error, UrlPolicyError::HostNotAllowed { .. }));
    }

    #[test]
    fn widening_the_allowlist_is_explicit_at_the_call_site() {
        let list = DomainAllowlist::empty().allowing("Media.ForgeCDN.net");
        assert!(list.permits_host("media.forgecdn.net"));
    }

    #[test]
    fn redaction_removes_query_fragment_and_credentials() {
        let url = Url::parse("https://user:secret@cdn.example/a.jar?token=abc#frag")
            .expect("test url parses");
        assert_eq!(redacted(&url), "https://cdn.example/a.jar");
    }

    #[test]
    fn transport_checks_allow_loopback_http_only_when_asked() {
        let url = Url::parse("http://127.0.0.1:9/a.jar").expect("test url parses");
        assert!(check_transport(&url, false).is_err());
        assert!(check_transport(&url, true).is_ok());
    }

    #[test]
    fn the_loopback_constructor_allows_http_and_an_ephemeral_port() {
        let list = DomainAllowlist::for_loopback_tests("127.0.0.1");
        assert!(list.validate("http://127.0.0.1:49152/a.jar").is_ok());
        assert!(list.validate("http://localhost:49152/a.jar").is_err());
    }
}
