//! The two Xbox Live legs: a user token, then an XSTS token for Minecraft.
//!
//! # Verified against the Minecraft Wiki's protocol page on 2026-08-08
//!
//! Microsoft publishes no protocol reference for this half of the chain — it is
//! an internal Xbox service that Minecraft happens to sit behind — so the shape
//! below was taken from the community protocol documentation and cross-checked
//! against a second independent implementation. Both agree on every field name.
//!
//! ```text
//! POST https://user.auth.xboxlive.com/user/authenticate
//! {"Properties":{"AuthMethod":"RPS","SiteName":"user.auth.xboxlive.com",
//!                "RpsTicket":"d=<microsoft access token>"},
//!  "RelyingParty":"http://auth.xboxlive.com","TokenType":"JWT"}
//!
//! POST https://xsts.auth.xboxlive.com/xsts/authorize
//! {"Properties":{"SandboxId":"RETAIL","UserTokens":["<xbl token>"]},
//!  "RelyingParty":"rp://api.minecraftservices.com/","TokenType":"JWT"}
//! ```
//!
//! Both answer with `Token` and `DisplayClaims.xui[0].uhs`.
//!
//! # Three details that are easy to get wrong
//!
//! **The `d=` prefix on `RpsTicket` is load-bearing.** It marks the ticket as
//! coming from the Microsoft identity platform rather than from the older
//! Windows Live endpoint. Sending the raw token gets a 400 that says nothing
//! about why.
//!
//! **The two legs have different relying parties.** The user token is generic
//! (`http://auth.xboxlive.com`); the XSTS token is minted specifically for
//! `rp://api.minecraftservices.com/`, trailing slash included. Reusing the
//! first value for the second produces a token Minecraft Services rejects.
//!
//! **A 401 from XSTS is usually about the account, not the request.** It
//! carries an `XErr` saying whether there is no Xbox profile, whether the
//! account is a child account, or whether Xbox Live is unavailable in that
//! country — each of which needs the user to go and do something different.
//! Reporting them all as "authorization failed" wastes an afternoon, so
//! [`crate::error::XstsDenial`] decodes them.

use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

use crate::endpoints::{Endpoints, MINECRAFT_RELYING_PARTY, XBOX_RELYING_PARTY};
use crate::error::{AuthError, AuthResult, XstsDenial};
use crate::http::send;
use crate::secret::SecretString;
use crate::stage::AuthStage;

/// An Xbox Live token plus the user hash that has to travel with it.
#[derive(Debug)]
pub struct XboxToken {
    pub token: SecretString,
    /// The `uhs` display claim. Not a secret on its own, but useless without
    /// the token and meaningless to anyone else, so it is kept beside it.
    pub user_hash: String,
}

impl XboxToken {
    /// The `identityToken` Minecraft Services expects:
    /// `XBL3.0 x=<user hash>;<xsts token>`.
    #[must_use]
    pub fn identity_token(&self) -> SecretString {
        SecretString::new(format!(
            "XBL3.0 x={};{}",
            self.user_hash,
            self.token.expose()
        ))
    }
}

/// The Xbox Live client.
#[derive(Debug, Clone)]
pub struct XboxClient {
    http: reqwest::Client,
    endpoints: Endpoints,
}

impl XboxClient {
    #[must_use]
    pub const fn new(http: reqwest::Client, endpoints: Endpoints) -> Self {
        Self { http, endpoints }
    }

    /// Exchanges a Microsoft access token for an Xbox Live user token.
    #[instrument(skip_all)]
    pub async fn authenticate(
        &self,
        microsoft_access_token: &SecretString,
        cancel: &CancellationToken,
    ) -> AuthResult<XboxToken> {
        let url = self.endpoints.xbox_authenticate()?;
        let body = serde_json::json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                // The `d=` prefix is required for identity-platform tokens.
                "RpsTicket": format!("d={}", microsoft_access_token.expose()),
            },
            "RelyingParty": XBOX_RELYING_PARTY,
            "TokenType": "JWT",
        });

        let request = self
            .http
            .post(url.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&body);
        let response = send(AuthStage::XboxLive, request, &url, cancel).await?;
        if !response.is_success() {
            return Err(AuthError::XboxAuthenticationFailed {
                status: response.status,
            });
        }

        response
            .parse::<XboxResponse>(AuthStage::XboxLive)?
            .into_token()
    }

    /// Exchanges an Xbox Live user token for an XSTS token scoped to Minecraft.
    #[instrument(skip_all)]
    pub async fn authorize(
        &self,
        user_token: &XboxToken,
        cancel: &CancellationToken,
    ) -> AuthResult<XboxToken> {
        let url = self.endpoints.xsts_authorize()?;
        let body = serde_json::json!({
            "Properties": {
                "SandboxId": "RETAIL",
                "UserTokens": [user_token.token.expose()],
            },
            "RelyingParty": MINECRAFT_RELYING_PARTY,
            "TokenType": "JWT",
        });

        let request = self
            .http
            .post(url.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&body);
        let response = send(AuthStage::Xsts, request, &url, cancel).await?;

        if response.status == 401 {
            // The interesting case: the body says *why*, and the reason is
            // almost always about the account rather than the request.
            let xerr = response
                .parse::<XstsErrorBody>(AuthStage::Xsts)
                .ok()
                .and_then(|body| body.xerr);
            return Err(AuthError::XstsDenied {
                reason: xerr.map_or(XstsDenial::Unknown, XstsDenial::from_xerr),
                xerr,
            });
        }
        if !response.is_success() {
            return Err(AuthError::HttpStatus {
                stage: AuthStage::Xsts,
                status: response.status,
            });
        }

        response
            .parse::<XboxResponse>(AuthStage::Xsts)?
            .into_token()
    }
}

/// The shape both Xbox endpoints answer with.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct XboxResponse {
    token: SecretString,
    display_claims: DisplayClaims,
}

#[derive(Debug, Deserialize)]
struct DisplayClaims {
    #[serde(default)]
    xui: Vec<XuiClaim>,
}

#[derive(Debug, Deserialize)]
struct XuiClaim {
    #[serde(default)]
    uhs: String,
}

impl XboxResponse {
    fn into_token(self) -> AuthResult<XboxToken> {
        let user_hash = self
            .display_claims
            .xui
            .into_iter()
            .map(|claim| claim.uhs)
            .find(|hash| !hash.is_empty())
            // Without the hash the next leg cannot build an identity token, so
            // continuing would only move the failure somewhere less explicable.
            .ok_or(AuthError::XboxUserHashMissing)?;
        Ok(XboxToken {
            token: self.token,
            user_hash,
        })
    }
}

/// The 401 body XSTS returns. Only `XErr` is read; the rest is a redirect URL
/// and a message that repeats it.
#[derive(Debug, Deserialize)]
struct XstsErrorBody {
    #[serde(rename = "XErr", default)]
    xerr: Option<u64>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// The live response shape, reduced to the fields the parser reads.
    const XBOX_RESPONSE: &str = r#"{
        "IssueInstant": "2026-08-08T12:00:00.0000000Z",
        "NotAfter": "2026-08-09T12:00:00.0000000Z",
        "Token": "eyJlbmMiOiJBMTI4Q0JDK0hTMjU2Iiwi",
        "DisplayClaims": {"xui": [{"uhs": "1503216055647848448"}]}
    }"#;

    #[test]
    fn an_xbox_response_yields_a_token_and_the_user_hash() {
        let parsed: XboxResponse = serde_json::from_str(XBOX_RESPONSE).expect("parses");
        let token = parsed.into_token().expect("a token");
        assert_eq!(token.user_hash, "1503216055647848448");
        assert_eq!(token.token.expose(), "eyJlbmMiOiJBMTI4Q0JDK0hTMjU2Iiwi");
    }

    #[test]
    fn a_response_with_no_user_hash_fails_here_rather_than_later() {
        // Minecraft Services would reject the resulting `XBL3.0 x=;<token>`
        // with a status that says nothing about the missing claim.
        for body in [
            r#"{"Token":"t","DisplayClaims":{"xui":[]}}"#,
            r#"{"Token":"t","DisplayClaims":{"xui":[{"uhs":""}]}}"#,
        ] {
            let parsed: XboxResponse = serde_json::from_str(body).expect("parses");
            assert!(matches!(
                parsed.into_token().expect_err("must fail"),
                AuthError::XboxUserHashMissing
            ));
        }
    }

    #[test]
    fn the_identity_token_is_the_format_minecraft_services_documents() {
        let token = XboxToken {
            token: SecretString::new("XSTS-TOKEN"),
            user_hash: "1503216055647848448".to_owned(),
        };
        assert_eq!(
            token.identity_token().expose(),
            "XBL3.0 x=1503216055647848448;XSTS-TOKEN"
        );
    }

    #[test]
    fn an_xbox_token_never_renders_its_token_but_keeps_the_hash_visible() {
        let token = XboxToken {
            token: SecretString::new("SECRET-JWT"),
            user_hash: "1503216055647848448".to_owned(),
        };
        let rendered = format!("{token:?}");
        assert!(!rendered.contains("SECRET-JWT"), "{rendered}");
        // The hash is what makes a log line about a failed exchange useful.
        assert!(rendered.contains("1503216055647848448"));
    }

    #[test]
    fn an_xsts_denial_body_decodes_the_reason() {
        let body: XstsErrorBody = serde_json::from_str(
            r#"{"Identity":"0","XErr":2148916233,"Message":"","Redirect":"https://start.ui.xboxlive.com/CreateAccount"}"#,
        )
.expect("parses");
        assert_eq!(body.xerr, Some(2_148_916_233));
        assert_eq!(
            XstsDenial::from_xerr(body.xerr.unwrap()),
            XstsDenial::NoXboxAccount
        );
    }

    #[test]
    fn a_denial_body_with_no_xerr_still_produces_a_usable_refusal() {
        let body: XstsErrorBody = serde_json::from_str("{}").expect("parses");
        assert!(body.xerr.is_none());
        assert_eq!(
            body.xerr.map_or(XstsDenial::Unknown, XstsDenial::from_xerr),
            XstsDenial::Unknown
        );
    }

    #[test]
    fn the_two_legs_use_different_relying_parties() {
        // Reusing the first for the second mints a token Minecraft Services
        // will not accept, and the failure surfaces a leg later.
        assert_ne!(XBOX_RELYING_PARTY, MINECRAFT_RELYING_PARTY);
        assert!(MINECRAFT_RELYING_PARTY.ends_with('/'));
    }
}
