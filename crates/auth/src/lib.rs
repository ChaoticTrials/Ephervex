//! Microsoft/Xbox/Minecraft Services authentication behind an
//! [`AccountProvider`] interface, with secrets held in the desktop keyring
//! (ADR 0004, ADR 0005).
//!
//! # The question this crate answers
//!
//! *Is this person allowed to launch Minecraft, and what does the game need to
//! be told about them?* Everything here serves that, and the answer is never
//! approximate: a launcher that starts the game for an account which cannot
//! play produces a disconnect at the title screen with no explanation
//!
//! # The chain, verified against live documentation on 2026-08-08
//!
//! Every URL, field name, and error code below was checked against the current
//! official sources on **2026-08-08**.
//!
//! ```text
//! 1. POST login.microsoftonline.com/consumers/oauth2/v2.0/{authorize,devicecode,token}
//! 2. POST user.auth.xboxlive.com/user/authenticate      -> Xbox Live user token
//! 3. POST xsts.auth.xboxlive.com/xsts/authorize         -> XSTS token for Minecraft
//! 4. POST api.minecraftservices.com/authentication/login_with_xbox
//! 5. GET  api.minecraftservices.com/entitlements/mcstore
//! 6. GET  api.minecraftservices.com/minecraft/profile
//! ```
//!
//! ## Sources consulted, 2026-08-08
//!
//! | Source | What it settled |
//! |---|---|
//! | Microsoft Learn, *OAuth 2.0 authorization code flow* (doc updated 2026-01-09) | authorize/token parameters, PKCE fields, refresh grant, the rule that a public client sends no `client_secret`, `http://localhost` as the native redirect URI |
//! | Microsoft Learn, *OAuth 2.0 device authorization grant* (doc updated 2025-01-04) | `/devicecode` takes `client_id` and `scope` **only**; the four polling errors; the 15-minute default `expires_in`; `verification_uri_complete` is *not* supported |
//! | Microsoft Learn, *Authentication flow support in MSAL* | the device flow is public-client only, i.e. it needs `allowPublicClient` |
//! | Minecraft Wiki, *Microsoft authentication* | the Xbox and Minecraft Services half: bodies, `RpsTicket` `d=` prefix, relying parties, `XErr` codes, `entitlements/mcstore`, the `404 NOT_FOUND` profile signal, and the Game Pass caveat |
//!
//! ## Upstream contract notes
//!
//! * Microsoft documents desktop-app configuration but leaves
//!   the Xbox/Minecraft half as "verify against current upstream documentation". It is verified
//!   and recorded here and in [`endpoints`], [`xbox`], and [`minecraft`].
//! * Ownership turned out to be **two** signals rather than one, and they
//!   disagree for Game Pass subscribers. See [`minecraft`] for which one
//!   decides and why the other is still asked for.
//! * The tenant is `consumers`, not `common`. Only a personal Microsoft account
//!   can hold the Xbox Live profile Minecraft requires.
//!
//! # The Azure application must be approved before any of this works
//!
//! Registering an application is not sufficient. Microsoft gates the Minecraft
//! Services API separately: *"new created Azure Apps must apply for the
//! Permission to use the Minecraft API … If your App don't have the Permission
//! `api.minecraftservices.com` will return a 403."* The form is
//! [`MINECRAFT_API_REVIEW_FORM`], and reported turnaround is weeks to months.
//!
//! The shape of that failure is worth knowing, because it is misleading. The
//! Microsoft, Xbox Live, and XSTS legs all succeed; the chain gets as far as
//! `login_with_xbox` and only then receives a `403` from a host that has
//! answered nothing else yet. It looks exactly like a Minecraft outage, and
//! reporting it as one — "try again in a few minutes" — sends the user into a
//! retry loop that cannot terminate. Hence
//! [`AuthError::MinecraftApiNotApproved`], which is a distinct variant with a
//! distinct message, no retry advice, and the Client ID the form asks for.
//!
//! # Reading the crate
//!
//! | Module | Holds |
//! |---|---|
//! | [`secret`] | [`SecretString`] — the type that makes a token unloggable |
//! | [`error`] | the typed errors, and which of them a re-login would fix |
//! | [`stage`] | which leg of the chain a message or failure is about |
//! | [`endpoints`] | every URL, and the scopes and relying parties |
//! | [`pkce`] | verifier, challenge, and the `state` value |
//! | [`loopback`] | the one-shot local listener that catches the redirect |
//! | [`microsoft`] | the identity platform: PKCE, device code, refresh |
//! | [`xbox`] | Xbox Live user token, then XSTS |
//! | [`minecraft`] | game token, entitlements, profile |
//! | [`offline`] | local identities, and the UUID a server would give them |
//! | [`store`] | keyring or session-only, and nothing else |
//! | [`provider`] | the [`AccountProvider`] interface and its event surface |
//! | [`chain`] | the implementation that runs all of it |
//!
//! # Three things that are easy to get wrong
//!
//! **The `d=` prefix on `RpsTicket`.** Xbox Live needs the Microsoft access
//! token marked as coming from the identity platform. Without it, a 400 that
//! explains nothing. See [`xbox`].
//!
//! **A refresh token may be replaced on every use.** Microsoft documents that
//! the client must discard the old one. A resume that stores nothing works for
//! weeks and then strands the user. See [`chain`].
//!
//! **A Game Pass account can be entitled and have no profile.** It needs the
//! official launcher run once. Reporting "you do not own Minecraft" would be
//! false. See [`minecraft`].
//!
//! **Not every non-2xx is worth retrying, and most of these are not.** A `403`
//! is permanent until an approval form is processed; an XSTS `XErr` is
//! permanent until the user changes something about their Xbox account. Only
//! `5xx`, `408`, `425`, and `429` justify "try again", which is why
//! [`AuthError::message_id`] picks between two ids on the status rather than
//! carrying one sentence per variant. See [`error`].
//!
//! # What this crate does not do
//!
//! * **It does not open a browser.** [`provider::AuthEvent::OpenBrowser`]
//!   carries the URL out to the frontend, which decides whether to hand it to
//!   the desktop, print it, or show it in a window. A headless caller has no
//!   desktop to hand it to.
//! * **It does not decide where secrets go.** The caller supplies a
//!   [`store::CredentialStore`], and the only two that exist are the keyring
//!   and memory. There is no file-backed one to fall back to on a bad day
//!   (ADR 0004).
//! * **It does not read or write configuration.** The Client ID arrives as an
//!   argument; `ephervex-core` resolves it from the user's setting or from
//!   [`DEFAULT_MICROSOFT_CLIENT_ID`].
//! * **It does not launch anything.** Vanilla launch support builds the command line;
//!   [`provider::Session`] is the account name, UUID, and access token it will
//!   need.

pub mod chain;
pub mod endpoints;
pub mod error;
pub mod http;
pub mod loopback;
pub mod microsoft;
pub mod minecraft;
pub mod offline;
pub mod pkce;
pub mod provider;
pub mod secret;
pub mod stage;
pub mod store;
pub mod xbox;

pub use chain::{MicrosoftAccountProvider, curseforge_api_key, set_curseforge_api_key};
pub use endpoints::{Endpoints, SCOPE, TENANT};
pub use error::{
    AuthError, AuthResult, CredentialOperation, MINECRAFT_API_REVIEW_FORM, XstsDenial,
};
pub use microsoft::{DeviceCodePrompt, MicrosoftClient};
pub use minecraft::{Entitlements, MinecraftProfile};
pub use offline::{OfflineAccount, offline_uuid};
pub use pkce::Pkce;
pub use provider::{
    Account, AccountProvider, AuthEvent, AuthObserver, Credentials, LoginFlow, LoginRequest,
    Session,
};
pub use secret::{REDACTED, SecretString};
pub use stage::AuthStage;
pub use store::{CredentialKey, CredentialStorage, CredentialStore, KeyringStore, MemoryStore};

/// The Azure application registration Ephervex authenticates as by default.
///
/// # This is a public identifier, not a secret
///
/// Ephervex is registered as a **public/native client**, which is the shape
/// Microsoft prescribes for desktop applications. Such a
/// registration has **no client secret**, and cannot have one: the binary ships
/// to users, so anything compiled into it is public by definition. The Client
/// ID identifies the application to Microsoft; it authenticates nothing, and
/// possessing it grants nobody anything. That is precisely why PKCE is not
/// optional here — see [`pkce`].
///
/// It is *not* Prism Launcher's, or any other launcher's. Ephervex does not allow
/// reusing another project's credentials, and doing so would put this
/// application's traffic under someone else's consent grants and rate limits.
///
/// # Overriding it
///
/// Two ways, in decreasing order of how often they matter:
///
/// * **At runtime**, via the `Microsoft Client ID` setting. A developer
///   running their own Azure registration sets it there, and it wins over this.
/// * **At build time**, via `EPHERVEX_MICROSOFT_CLIENT_ID`, so a fork can ship
///   its own default without patching this file.
pub const DEFAULT_MICROSOFT_CLIENT_ID: &str = match option_env!("EPHERVEX_MICROSOFT_CLIENT_ID") {
    Some(id) => id,
    None => "767ea465-0bba-443b-a06b-831c71777b90",
};

/// Resolves the Client ID to authenticate with.
///
/// The user's setting wins; an empty or whitespace-only setting means "use the
/// build's default", which is what an empty text field in a settings screen
/// has to mean.
#[must_use]
pub fn resolve_client_id(configured: &str) -> &str {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        DEFAULT_MICROSOFT_CLIENT_ID
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_default_client_id_is_a_uuid_and_is_not_empty() {
        assert!(!DEFAULT_MICROSOFT_CLIENT_ID.is_empty());
        assert!(
            uuid::Uuid::parse_str(DEFAULT_MICROSOFT_CLIENT_ID).is_ok(),
            "an application id that is not a GUID would be rejected by Microsoft"
        );
    }

    #[test]
    fn a_configured_client_id_outranks_the_build_default() {
        let configured = "11112222-bbbb-3333-cccc-4444dddd5555";
        assert_eq!(resolve_client_id(configured), configured);
        assert_eq!(
            resolve_client_id("  11112222-bbbb-3333-cccc-4444dddd5555  "),
            configured
        );
    }

    #[test]
    fn an_empty_setting_falls_back_to_the_build_default() {
        // An empty text field in a settings screen means "use the default", not
        // "authenticate as nobody".
        assert_eq!(resolve_client_id(""), DEFAULT_MICROSOFT_CLIENT_ID);
        assert_eq!(resolve_client_id("   \t "), DEFAULT_MICROSOFT_CLIENT_ID);
    }
}
