//! The service addresses the chain talks to.
//!
//! # Verified against live documentation on 2026-08-08
//!
//! Ephervex requires not to trust its own 2026 snapshot, so every address and
//! every field name below was re-read from the current official documentation
//! on **2026-08-08**. See the crate-level docs in [`crate`] for the sources and
//! for what actually differed.
//!
//! # Why the bases are injectable but not configurable
//!
//! [`Endpoints::with_base`] points the whole chain at one host, which is what
//! the offline test suite needs. It is deliberately not exposed as a setting:
//! redirecting the token endpoint is how you collect somebody's Microsoft
//! credentials, and no user-facing knob should make that a typo away.
//!
//! The Client ID *is* a setting, because it is not a secret and
//! because a developer running their own Azure registration is a supported
//! thing to do.
//!
//! # Bases are held as text, not as parsed URLs
//!
//! Every accessor here returns a `Result`, so parsing at construction would
//! only move the same failure earlier while forcing the infallible
//! [`Endpoints::production`] constructor to invent a URL it could not have.

use reqwest::Url;

use crate::error::AuthError;

/// The Microsoft identity platform host.
pub const MICROSOFT_IDENTITY_BASE: &str = "https://login.microsoftonline.com";

/// The Xbox Live user authentication host.
pub const XBOX_USER_BASE: &str = "https://user.auth.xboxlive.com";

/// The Xbox Secure Token Service host.
pub const XBOX_XSTS_BASE: &str = "https://xsts.auth.xboxlive.com";

/// Minecraft Services.
pub const MINECRAFT_SERVICES_BASE: &str = "https://api.minecraftservices.com";

/// The tenant segment of the Microsoft identity URLs.
///
/// `consumers` rather than `common`: only a personal Microsoft account can hold
/// an Xbox Live profile, and only an Xbox Live profile can own Minecraft Java
/// Edition. `common` would happily sign a work or school account in, and the
/// failure would then surface two legs later as an opaque Xbox Live refusal
/// with no hint that the account type was the problem.
pub const TENANT: &str = "consumers";

/// The OAuth scopes Minecraft authentication needs.
///
/// `XboxLive.signin` is what makes the resulting token usable as an Xbox Live
/// RPS ticket; without it the Xbox leg fails unhelpfully. `offline_access` is
/// what makes Microsoft issue a refresh token at all, and without it "app
/// restart preserves login" — a account and authentication support required behavior — is impossible.
pub const SCOPE: &str = "XboxLive.signin offline_access";

/// The relying party the XSTS token is minted for.
///
/// The trailing slash is part of the value Minecraft Services expects.
pub const MINECRAFT_RELYING_PARTY: &str = "rp://api.minecraftservices.com/";

/// The relying party of the *first* Xbox leg, which is not the same as the one
/// above: the user token is generic, the XSTS token is Minecraft-specific.
pub const XBOX_RELYING_PARTY: &str = "http://auth.xboxlive.com";

/// Where the chain sends its requests.
#[derive(Debug, Clone)]
pub struct Endpoints {
    microsoft: String,
    xbox_user: String,
    xbox_xsts: String,
    minecraft: String,
    tenant: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self::production()
    }
}

impl Endpoints {
    /// The real services.
    #[must_use]
    pub fn production() -> Self {
        Self {
            microsoft: MICROSOFT_IDENTITY_BASE.to_owned(),
            xbox_user: XBOX_USER_BASE.to_owned(),
            xbox_xsts: XBOX_XSTS_BASE.to_owned(),
            minecraft: MINECRAFT_SERVICES_BASE.to_owned(),
            tenant: TENANT.to_owned(),
        }
    }

    /// Points every leg of the chain at one host.
    ///
    /// For the offline test suite, which serves all five services from a single
    /// `wiremock` instance. Not a setting; see the module docs.
    #[must_use]
    pub fn with_base(base: &str) -> Self {
        Self {
            microsoft: base.to_owned(),
            xbox_user: base.to_owned(),
            xbox_xsts: base.to_owned(),
            minecraft: base.to_owned(),
            tenant: TENANT.to_owned(),
        }
    }

    /// `https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize`.
    pub fn authorize(&self) -> Result<Url, AuthError> {
        self.microsoft_path("authorize")
    }

    /// `https://login.microsoftonline.com/consumers/oauth2/v2.0/token`.
    pub fn token(&self) -> Result<Url, AuthError> {
        self.microsoft_path("token")
    }

    /// `https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode`.
    pub fn device_code(&self) -> Result<Url, AuthError> {
        self.microsoft_path("devicecode")
    }

    /// `https://user.auth.xboxlive.com/user/authenticate`.
    pub fn xbox_authenticate(&self) -> Result<Url, AuthError> {
        absolute(&self.xbox_user, "user/authenticate")
    }

    /// `https://xsts.auth.xboxlive.com/xsts/authorize`.
    pub fn xsts_authorize(&self) -> Result<Url, AuthError> {
        absolute(&self.xbox_xsts, "xsts/authorize")
    }

    /// `https://api.minecraftservices.com/authentication/login_with_xbox`.
    pub fn minecraft_login(&self) -> Result<Url, AuthError> {
        absolute(&self.minecraft, "authentication/login_with_xbox")
    }

    /// `https://api.minecraftservices.com/entitlements/mcstore`.
    pub fn entitlements(&self) -> Result<Url, AuthError> {
        absolute(&self.minecraft, "entitlements/mcstore")
    }

    /// `https://api.minecraftservices.com/minecraft/profile`.
    pub fn profile(&self) -> Result<Url, AuthError> {
        absolute(&self.minecraft, "minecraft/profile")
    }

    fn microsoft_path(&self, leaf: &str) -> Result<Url, AuthError> {
        absolute(
            &self.microsoft,
            &format!("{}/oauth2/v2.0/{leaf}", self.tenant),
        )
    }
}

/// Builds `<base>/<path>` without letting `Url::join`'s relative-reference
/// rules eat a segment when the base has no trailing slash — which is exactly
/// the shape `wiremock` hands out.
fn absolute(base: &str, path: &str) -> Result<Url, AuthError> {
    let joined = format!("{}/{path}", base.trim_end_matches('/'));
    Url::parse(&joined).map_err(|_| AuthError::EndpointUnusable { url: joined })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_production_endpoints_are_the_ones_verified_on_2026_08_08() {
        let endpoints = Endpoints::production();
        assert_eq!(
            endpoints.authorize().unwrap().as_str(),
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize"
        );
        assert_eq!(
            endpoints.token().unwrap().as_str(),
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/token"
        );
        assert_eq!(
            endpoints.device_code().unwrap().as_str(),
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode"
        );
        assert_eq!(
            endpoints.xbox_authenticate().unwrap().as_str(),
            "https://user.auth.xboxlive.com/user/authenticate"
        );
        assert_eq!(
            endpoints.xsts_authorize().unwrap().as_str(),
            "https://xsts.auth.xboxlive.com/xsts/authorize"
        );
        assert_eq!(
            endpoints.minecraft_login().unwrap().as_str(),
            "https://api.minecraftservices.com/authentication/login_with_xbox"
        );
        assert_eq!(
            endpoints.entitlements().unwrap().as_str(),
            "https://api.minecraftservices.com/entitlements/mcstore"
        );
        assert_eq!(
            endpoints.profile().unwrap().as_str(),
            "https://api.minecraftservices.com/minecraft/profile"
        );
    }

    #[test]
    fn every_production_endpoint_is_https() {
        // HTTPS only. A token endpoint reached over plaintext is a
        // credential handed to whoever is on the path.
        let endpoints = Endpoints::production();
        for url in [
            endpoints.authorize().unwrap(),
            endpoints.token().unwrap(),
            endpoints.device_code().unwrap(),
            endpoints.xbox_authenticate().unwrap(),
            endpoints.xsts_authorize().unwrap(),
            endpoints.minecraft_login().unwrap(),
            endpoints.entitlements().unwrap(),
            endpoints.profile().unwrap(),
        ] {
            assert_eq!(url.scheme(), "https", "{url}");
        }
    }

    #[test]
    fn a_test_base_without_a_trailing_slash_still_produces_whole_paths() {
        // `wiremock` hands out `http://127.0.0.1:34567` with no trailing slash,
        // and `Url::join` would otherwise replace the last segment rather than
        // append to it.
        let endpoints = Endpoints::with_base("http://127.0.0.1:34567");
        assert_eq!(
            endpoints.token().unwrap().path(),
            "/consumers/oauth2/v2.0/token"
        );
        assert_eq!(endpoints.profile().unwrap().path(), "/minecraft/profile");
    }

    #[test]
    fn a_base_that_is_not_a_url_is_refused_rather_than_silently_dropped() {
        let error = Endpoints::with_base("not a url")
            .token()
            .expect_err("must refuse");
        assert!(matches!(error, AuthError::EndpointUnusable { .. }));
    }

    #[test]
    fn the_tenant_is_consumers_because_only_personal_accounts_own_the_game() {
        assert_eq!(TENANT, "consumers");
        assert!(
            Endpoints::production()
                .authorize()
                .unwrap()
                .path()
                .contains("/consumers/")
        );
    }

    #[test]
    fn the_scope_asks_for_both_xbox_sign_in_and_a_refresh_token() {
        assert!(SCOPE.contains("XboxLive.signin"));
        // Without `offline_access` there is no refresh token, and "app restart
        // preserves login" becomes impossible.
        assert!(SCOPE.contains("offline_access"));
    }
}
