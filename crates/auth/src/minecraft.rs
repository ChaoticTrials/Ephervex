//! Minecraft Services: the game token, the ownership check, and the profile.
//!
//! # Verified against the Minecraft protocol documentation on 2026-08-08
//!
//! ```text
//! POST /authentication/login_with_xbox   {"identityToken": "XBL3.0 x=<uhs>;<xsts>"}
//!   -> {"access_token": "...", "expires_in": 86400, "token_type": "Bearer"}
//! GET  /entitlements/mcstore             Authorization: Bearer <access token>
//!   -> {"items":[{"name":"product_minecraft","signature":"..."},...]}
//! GET  /minecraft/profile                Authorization: Bearer <access token>
//!   -> {"id":"<32 hex chars, unhyphenated>","name":"...","skins":[...]}
//! ```
//!
//! # What "verify ownership" actually means, and why it is two calls
//!
//! Ownership must be verified before launch, and there are
//! two candidate signals. They disagree, and the disagreement is the whole
//! point:
//!
//! * `/entitlements/mcstore` lists what the account is entitled to —
//!   `product_minecraft` and `game_minecraft` for a purchase,
//!   `product_game_pass_pc` or `product_game_pass_ultimate` for a subscription.
//! * `/minecraft/profile` answers `404` with `"error": "NOT_FOUND"` when there
//!   is no Java Edition profile.
//!
//! The profile is the authoritative signal for *launching*, because the launch
//! arguments need the profile's UUID and name and there is nothing to substitute
//! if they are absent. But the entitlements are what make the failure
//! explicable: a Game Pass subscriber who has never opened the official
//! launcher **has an entitlement and no profile**, and telling them they do not
//! own Minecraft would be wrong. So the chain asks both, treats the profile as
//! deciding, and uses the entitlements to say *which* refusal this is
//!
//! # The profile UUID arrives without hyphens
//!
//! `069a79f4-44e9-4726-a5be-fca90e38aaf5` is published as
//! `069a79f444e94726a5befca90e38aaf5`. `Uuid::parse_str` accepts both, so this
//! is only a trap for hand-rolled parsing — but the value that reaches the
//! launch arguments has to be the hyphenated form.

use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument};
use uuid::Uuid;

use crate::endpoints::Endpoints;
use crate::error::{AuthError, AuthResult};
use crate::http::send;
use crate::secret::SecretString;
use crate::stage::AuthStage;

/// Entitlement names that mean the account bought the game outright.
pub const PURCHASE_ENTITLEMENTS: &[&str] = &["product_minecraft", "game_minecraft"];

/// Entitlement names that mean the account reaches the game through Game Pass.
///
/// Kept separate because these accounts need the official launcher run once
/// before a Java Edition profile exists, which is a different instruction from
/// "buy the game".
pub const GAME_PASS_ENTITLEMENTS: &[&str] = &["product_game_pass_pc", "product_game_pass_ultimate"];

/// A Minecraft Services access token and when it stops working.
#[derive(Debug)]
pub struct MinecraftToken {
    pub access: SecretString,
    /// Seconds the service said it is valid for. 86 400 in practice.
    pub expires_in: i64,
}

/// What the account is entitled to.
#[derive(Debug, Clone, Default)]
pub struct Entitlements {
    /// Raw entitlement names, in the order the store reported them. Not
    /// interpreted beyond the two lists above: Mojang adds names over time and
    /// an unknown one is information, not an error.
    pub items: Vec<String>,
}

impl Entitlements {
    /// Whether the account bought Minecraft Java Edition.
    #[must_use]
    pub fn owns_game(&self) -> bool {
        self.matches(PURCHASE_ENTITLEMENTS)
    }

    /// Whether the account reaches the game through Game Pass.
    #[must_use]
    pub fn has_game_pass(&self) -> bool {
        self.matches(GAME_PASS_ENTITLEMENTS)
    }

    /// Whether anything here permits playing at all.
    #[must_use]
    pub fn permits_play(&self) -> bool {
        self.owns_game() || self.has_game_pass()
    }

    fn matches(&self, names: &[&str]) -> bool {
        self.items
            .iter()
            .any(|item| names.iter().any(|name| item == name))
    }
}

/// The account as Minecraft knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinecraftProfile {
    pub id: Uuid,
    pub name: String,
}

/// The status Minecraft Services returns when the calling Azure application has
/// not been approved for the API.
///
/// Not a rejection of the user, the account, or the identity token — a
/// rejection of the *application registration*. See
/// [`AuthError::MinecraftApiNotApproved`].
const NOT_APPROVED: u16 = 403;

/// The Minecraft Services client.
#[derive(Debug, Clone)]
pub struct MinecraftServicesClient {
    http: reqwest::Client,
    endpoints: Endpoints,
    /// The Azure application these requests are made as.
    ///
    /// Carried by an otherwise stateless API client for exactly one reason: a
    /// `403` from any of these endpoints means *this* application is not
    /// approved, and the approval form asks for the Client ID. Naming it in the
    /// error is the difference between a user who can act and one who has to go
    /// looking for a value the launcher already knew.
    client_id: String,
}

impl MinecraftServicesClient {
    #[must_use]
    pub fn new(http: reqwest::Client, endpoints: Endpoints, client_id: &str) -> Self {
        Self {
            http,
            endpoints,
            client_id: client_id.to_owned(),
        }
    }

    /// The refusal a `403` from any Minecraft Services endpoint means.
    fn not_approved(&self) -> AuthError {
        AuthError::MinecraftApiNotApproved {
            client_id: self.client_id.clone(),
        }
    }

    /// Exchanges an Xbox identity token for a Minecraft access token.
    #[instrument(skip_all)]
    pub async fn login(
        &self,
        identity_token: &SecretString,
        cancel: &CancellationToken,
    ) -> AuthResult<MinecraftToken> {
        let url = self.endpoints.minecraft_login()?;
        let request = self
            .http
            .post(url.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&serde_json::json!({"identityToken": identity_token.expose()}));

        let response = send(AuthStage::MinecraftLogin, request, &url, cancel).await?;
        // The first place an unapproved application finds out. Every endpoint
        // on this host answers 403 until the registration is approved, and the
        // chain reaches this one first.
        if response.status == NOT_APPROVED {
            return Err(self.not_approved());
        }
        if !response.is_success() {
            return Err(AuthError::MinecraftLoginRejected {
                status: response.status,
            });
        }

        let body: LoginBody = response.parse(AuthStage::MinecraftLogin)?;
        Ok(MinecraftToken {
            access: body.access_token,
            expires_in: body.expires_in,
        })
    }

    /// Reads what the account is entitled to.
    #[instrument(skip_all)]
    pub async fn entitlements(
        &self,
        token: &SecretString,
        cancel: &CancellationToken,
    ) -> AuthResult<Entitlements> {
        let url = self.endpoints.entitlements()?;
        let response = send(
            AuthStage::Entitlements,
            authorized(self.http.get(url.clone()), token),
            &url,
            cancel,
        )
        .await?;

        if response.status == NOT_APPROVED {
            return Err(self.not_approved());
        }
        if !response.is_success() {
            return Err(AuthError::HttpStatus {
                stage: AuthStage::Entitlements,
                status: response.status,
            });
        }

        let body: EntitlementsBody = response.parse(AuthStage::Entitlements)?;
        let items: Vec<String> = body.items.into_iter().map(|item| item.name).collect();
        // Entitlement names are not secrets and are exactly what a bug report
        // about "it says I do not own the game" needs to contain.
        debug!(?items, "read the entitlement store");
        Ok(Entitlements { items })
    }

    /// Reads the Java Edition profile.
    ///
    /// A `404` is not an error here: it is the documented way the service says
    /// this account has no Java Edition profile. The caller decides what that
    /// means, because the answer depends on the entitlements.
    #[instrument(skip_all)]
    pub async fn profile(
        &self,
        token: &SecretString,
        cancel: &CancellationToken,
    ) -> AuthResult<Option<MinecraftProfile>> {
        let url = self.endpoints.profile()?;
        let response = send(
            AuthStage::Profile,
            authorized(self.http.get(url.clone()), token),
            &url,
            cancel,
        )
        .await?;

        if response.status == 404 {
            return Ok(None);
        }
        // Checked before the generic branch: a 403 here is the application
        // registration, not the profile, and the two need opposite advice.
        if response.status == NOT_APPROVED {
            return Err(self.not_approved());
        }
        if !response.is_success() {
            return Err(AuthError::ProfileUnavailable {
                status: response.status,
            });
        }

        let body: ProfileBody = response.parse(AuthStage::Profile)?;
        // The service publishes the UUID unhyphenated; `parse_str` accepts both
        // spellings and the launch arguments need the hyphenated one.
        let id = Uuid::parse_str(&body.id).map_err(|_| AuthError::ProfileMalformed)?;
        Ok(Some(MinecraftProfile {
            id,
            name: body.name,
        }))
    }
}

/// Attaches the bearer token.
///
/// One place, so no request can be built that forgets it — and, more to the
/// point, so the header is never assembled somewhere that also logs.
fn authorized(request: reqwest::RequestBuilder, token: &SecretString) -> reqwest::RequestBuilder {
    request
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", token.expose()),
        )
        .header(reqwest::header::ACCEPT, "application/json")
}

#[derive(Debug, Deserialize)]
struct LoginBody {
    access_token: SecretString,
    #[serde(default)]
    expires_in: i64,
}

#[derive(Debug, Deserialize)]
struct EntitlementsBody {
    #[serde(default)]
    items: Vec<EntitlementItem>,
}

#[derive(Debug, Deserialize)]
struct EntitlementItem {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct ProfileBody {
    id: String,
    #[serde(default)]
    name: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn entitlements(names: &[&str]) -> Entitlements {
        Entitlements {
            items: names.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    #[test]
    fn a_purchase_and_a_subscription_are_told_apart() {
        let bought = entitlements(&["product_minecraft", "game_minecraft"]);
        assert!(bought.owns_game());
        assert!(!bought.has_game_pass());
        assert!(bought.permits_play());

        let subscribed = entitlements(&["product_game_pass_ultimate"]);
        assert!(!subscribed.owns_game());
        assert!(subscribed.has_game_pass());
        assert!(subscribed.permits_play());
    }

    #[test]
    fn an_account_with_nothing_permits_nothing() {
        assert!(!Entitlements::default().permits_play());
        // An unrelated entitlement is not an error and is not permission.
        let other = entitlements(&["product_minecraft_bedrock"]);
        assert!(!other.permits_play(), "prefix matching would be wrong here");
    }

    #[test]
    fn the_entitlement_document_parses_into_names_only() {
        // The signatures are JWTs Mojang uses to prove the entitlement to
        // itself; Ephervex neither validates nor stores them.
        let body: EntitlementsBody = serde_json::from_str(
            r#"{"items":[
                {"name":"product_minecraft","signature":"eyJhbGciOiJSUzI1NiJ9.redacted"},
                {"name":"game_minecraft","signature":"eyJhbGciOiJSUzI1NiJ9.redacted"}
            ],"signature":"eyJhbGciOiJSUzI1NiJ9.redacted","keyId":"1"}"#,
        )
        .expect("parses");
        let names: Vec<_> = body.items.into_iter().map(|item| item.name).collect();
        assert_eq!(names, ["product_minecraft", "game_minecraft"]);
    }

    #[test]
    fn an_unhyphenated_profile_id_becomes_a_real_uuid() {
        let body: ProfileBody = serde_json::from_str(
            r#"{"id":"069a79f444e94726a5befca90e38aaf5","name":"Notch","skins":[],"capes":[]}"#,
        )
        .expect("parses");
        let id = Uuid::parse_str(&body.id).expect("uuid");
        assert_eq!(id.to_string(), "069a79f4-44e9-4726-a5be-fca90e38aaf5");
        assert_eq!(body.name, "Notch");
    }

    #[test]
    fn a_profile_id_that_is_not_a_uuid_is_refused() {
        // The launch arguments carry this straight through to the game; a
        // malformed value would surface as an unexplained crash on start.
        assert!(Uuid::parse_str("not-a-uuid").is_err());
    }

    #[test]
    fn a_login_response_hides_the_token_but_keeps_the_lifetime() {
        let body: LoginBody = serde_json::from_str(
            r#"{"username":"a-uuid","roles":[],"access_token":"eyJhbGciOiJIUzI1NiJ9.SECRET",
                "token_type":"Bearer","expires_in":86400}"#,
        )
        .expect("parses");
        assert_eq!(body.expires_in, 86_400);
        assert!(!format!("{body:?}").contains("SECRET"));
    }
}
