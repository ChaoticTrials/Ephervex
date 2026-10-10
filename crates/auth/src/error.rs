//! Typed errors for authentication.
//!
//! No English prose lives in these variants. Each carries structured data plus
//! a stable [`AuthError::message_id`], and the frontend turns that into a
//! sentence through `ephervex-i18n`. The `Display` implementations exist for
//! logs and developer output.
//!
//! # The variants are shaped around what the user can do next
//!
//! Mechanically, "your refresh token expired", "you denied the consent prompt",
//! and "you do not own Minecraft" are all just non-success responses from the
//! same chain. They are completely different situations to be in: the first
//! wants a silent re-login, the second wants the flow started again, and the
//! third is not fixable from inside this launcher at all. Flattening them into
//! one `AuthFailed` would push that distinction into the frontend, which has
//! less information than this crate does.
//!
//! # Nothing here holds a token
//!
//! Not the access token, not the refresh token, not the authorization code, not
//! the `identityToken` header. Error values end up in logs by construction -
//! `{error:?}`, `#[instrument]`, `error.chain()` in the CLI - so a variant that
//! carried a credential would be a leak with extra steps.

use ephervex_download::status_is_transient;

use crate::stage::AuthStage;

/// Where an Azure application applies for access to the Minecraft Services API.
///
/// A named constant rather than a literal in a message, for the same reason
/// paths and version strings are. URLs stay out of translatable
/// prose, so this travels into the message as an argument. It is also the value
/// a frontend puts behind a clickable link, which a string embedded in a
/// sentence could not be.
pub const MINECRAFT_API_REVIEW_FORM: &str = "https://aka.ms/mce-reviewappid";

/// Anything the authentication chain can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// No Microsoft Client ID is configured and the build carries no default.
    #[error("no microsoft client id is configured")]
    ClientIdMissing,

    /// The configured Client ID is not an application id Microsoft could match.
    ///
    /// Refused at the settings boundary rather than at sign-in: Microsoft's own
    /// rejection arrives several requests into a login and blames the tenant,
    /// which sends the user looking in the wrong place.
    #[error("the configured microsoft application id is not well-formed")]
    ClientIdInvalid,

    /// An endpoint base could not be parsed as a URL.
    #[error("`{url}` is not a usable authentication endpoint")]
    EndpointUnusable { url: String },

    /// The HTTP client could not be constructed.
    #[error("could not build the http client")]
    ClientBuild {
        #[source]
        source: Box<ephervex_download::DownloadError>,
    },

    /// Connection, TLS, or timeout failure talking to one of the services.
    #[error("could not reach the {stage} service")]
    Transport {
        stage: AuthStage,
        #[source]
        source: Box<reqwest::Error>,
    },

    /// A service answered with a status the chain cannot continue from.
    ///
    /// Deliberately carries no body: Xbox and Minecraft Services both echo
    /// request material into error payloads.
    #[error("http {status} from the {stage} service")]
    HttpStatus { stage: AuthStage, status: u16 },

    /// A service answered with something that is not the documented shape.
    #[error("the {stage} service returned an unexpected document")]
    MalformedResponse {
        stage: AuthStage,
        #[source]
        source: Box<serde_json::Error>,
    },

    /// A service sent a body larger than any document the chain expects.
    ///
    /// Not transient: a retry would receive the same oversized document.
    #[error("the {stage} service returned more than {limit} bytes")]
    ResponseTooLarge { stage: AuthStage, limit: usize },

    /// Microsoft refused the request with an OAuth error code.
    ///
    /// `description` is Microsoft's own `error_description`, which carries the
    /// `AADSTS` code developers need. It never contains a token.
    #[error("microsoft rejected the request: {code}")]
    OAuth { code: String, description: String },

    /// The user declined the consent prompt, or closed it.
    #[error("the sign-in was declined")]
    Declined,

    /// The device code expired before the user finished signing in.
    #[error("the device code expired before sign-in completed")]
    DeviceCodeExpired,

    /// Nobody completed the browser flow within the deadline.
    #[error("no sign-in arrived within {seconds}s")]
    TimedOut { seconds: u64 },

    /// The redirect carried a `state` that is not the one this process sent.
    ///
    /// A cross-site request forgery attempt, or a stale browser tab from an
    /// earlier attempt. Either way the code that came with it is not ours.
    #[error("the sign-in redirect did not match this request")]
    StateMismatch,

    /// The loopback listener could not be bound, or died while waiting.
    #[error("the local sign-in listener could not be started")]
    RedirectListener {
        #[source]
        source: std::io::Error,
    },

    /// The caller's cancellation token fired. Distinct from a failure.
    #[error("the sign-in was canceled")]
    Canceled,

    /// Xbox Live refused to issue a user token.
    #[error("xbox live did not issue a user token (http {status})")]
    XboxAuthenticationFailed { status: u16 },

    /// XSTS refused authorization, usually for a reason about the account
    /// rather than about the request. See [`XstsDenial`].
    #[error("xbox live refused authorization: {reason}")]
    XstsDenied {
        reason: XstsDenial,
        /// Microsoft's raw `XErr`, preserved for the ones this build does not
        /// recognize yet.
        xerr: Option<u64>,
    },

    /// A token response omitted the user hash the next leg needs.
    #[error("the xbox live response carried no user hash")]
    XboxUserHashMissing,

    /// Minecraft Services refused the Xbox identity token.
    ///
    /// Everything except `403`, which is a different problem entirely - see
    /// [`Self::MinecraftApiNotApproved`].
    #[error("minecraft services rejected the xbox identity (http {status})")]
    MinecraftLoginRejected { status: u16 },

    /// The Azure application has not been approved for the Minecraft Services
    /// API, so every call to `api.minecraftservices.com` answers `403`.
    ///
    /// This is a property of the *application registration*, not of the user or
    /// their account, and no amount of signing in again will change it. It is
    /// separated from [`Self::MinecraftLoginRejected`] because the advice that
    /// fits a rejected identity - try again, check the account - sends a user
    /// hitting this one into a retry loop that can never terminate
    ///
    /// `client_id` is carried because the approval form asks for it, so the
    /// message can hand the user the exact value they have to paste.
    #[error(
        "the azure application `{client_id}` is not approved for the minecraft services api (http 403)"
    )]
    MinecraftApiNotApproved { client_id: String },

    /// The account authenticated but cannot play Minecraft Java Edition.
    ///
    /// `entitlements` records what the store *did* report, because "no
    /// entitlements at all" and "a Game Pass entitlement with no profile yet"
    /// are different problems with different fixes.
    #[error("this microsoft account cannot play minecraft java edition")]
    GameNotOwned {
        entitlements: Vec<String>,
        /// True when the store reported a Game Pass entitlement. Those accounts
        /// have no profile until the official launcher has run once.
        game_pass: bool,
    },

    /// There is no Java Edition profile and the entitlement store could not be
    /// read, so whether the account owns the game is unknown.
    ///
    /// Not [`AuthError::GameNotOwned`]: that is a definite answer with purchase
    /// advice, and an outage must not produce one.
    #[error("minecraft ownership could not be confirmed because the entitlement store failed")]
    GameOwnershipUnconfirmed,

    /// The profile endpoint failed for a reason other than "no such profile".
    #[error("the minecraft profile could not be read (http {status})")]
    ProfileUnavailable { status: u16 },

    /// The profile arrived but its id is not a UUID.
    #[error("the minecraft profile carried an unusable account id")]
    ProfileMalformed,

    /// Secure credential storage is not available on this machine.
    ///
    /// Never a silent fallback to a file. ADR 0004 makes
    /// this an explicit refusal that offers session-only login instead.
    #[error("secure credential storage is unavailable")]
    SecretStorageUnavailable { detail: String },

    /// Another process kept sign-in credentials locked for longer than a
    /// change waits.
    #[error("another process is changing sign-in credentials ({path})")]
    CredentialChangeBusy { path: std::path::PathBuf },

    /// The lock that orders sign-in credential changes could not be taken.
    #[error("sign-in credential changes could not be ordered ({path})")]
    CredentialChangeUnavailable { path: std::path::PathBuf },

    /// A credential was submitted with nothing but whitespace in it.
    ///
    /// Refused before it reaches the keyring rather than stored and discovered
    /// later: a blank entry would report as *configured* while every request
    /// made with it was rejected, which is the least diagnosable state to leave
    /// a user in.
    #[error("a blank value cannot be stored as a credential")]
    CredentialValueBlank,

    /// The keyring is present but the operation failed.
    #[error("the credential store could not {operation} the credential")]
    CredentialStore {
        operation: CredentialOperation,
        #[source]
        source: Box<keyring::Error>,
    },

    /// An offline account name Minecraft itself would refuse.
    ///
    /// Checked when the account is created rather than at launch: the name is
    /// hashed into the player's UUID, so an invalid one produces a failure much
    /// later wearing somebody else's stack trace.
    #[error("`{name}` is not a usable minecraft player name")]
    OfflineNameInvalid { name: String },

    /// An account was named that this installation does not have.
    #[error("no account matches `{reference}`")]
    AccountUnknown { reference: String },

    /// A name was given that more than one account answers to.
    #[error("`{reference}` matches {matches} accounts")]
    AccountAmbiguous { reference: String, matches: usize },

    /// An offline account already exists with this name.
    ///
    /// Refused rather than deduplicated: the UUID is derived from the name, so
    /// a second one would be the *same player*, and silently returning the
    /// existing account would hide that from someone who thought they were
    /// adding a second identity.
    #[error("an offline account named `{name}` already exists")]
    OfflineAccountExists { name: String },

    /// Something asked for a session and nobody is signed in.
    #[error("no account is signed in")]
    NotSignedIn,

    /// A stored credential signed in as a different account than the one it
    /// is kept for.
    ///
    /// Not recoverable by retrying: the session is refused rather than
    /// recorded under the wrong identity, and signing in again replaces the
    /// credential.
    #[error("the stored credential for account {requested} signed in as account {found}")]
    AccountMismatch {
        requested: uuid::Uuid,
        found: uuid::Uuid,
    },

    /// The stored refresh token is gone, revoked, or expired.
    ///
    /// the expired-token case once refreshing itself has failed: the
    /// only way out is an interactive sign-in.
    #[error("the stored microsoft credential is no longer valid")]
    ReauthenticationRequired { code: String },
}

/// Why XSTS refused, decoded from Microsoft's `XErr`.
///
/// The numeric codes are the ones Microsoft documents for Xbox Live sign-in.
/// Each is a genuinely different situation, and telling a user "authorization
/// failed" when the truth is "this account has no Xbox profile yet" wastes an
/// afternoon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum XstsDenial {
    /// `2148916227` - the account was banned from Xbox Live.
    AccountBanned,
    /// `2148916233` - the Microsoft account has no Xbox Live profile.
    NoXboxAccount,
    /// `2148916235` - Xbox Live is not available in this account's country.
    CountryUnavailable,
    /// `2148916236` / `2148916237` - proof of age is required.
    AdultVerificationRequired,
    /// `2148916238` - a child account must be added to a Family.
    ChildAccount,
    /// An `XErr` this build does not recognize, or none at all.
    Unknown,
}

impl XstsDenial {
    /// Decodes an `XErr` from an XSTS 401 body.
    #[must_use]
    pub const fn from_xerr(xerr: u64) -> Self {
        match xerr {
            2_148_916_227 => Self::AccountBanned,
            2_148_916_233 => Self::NoXboxAccount,
            2_148_916_235 => Self::CountryUnavailable,
            2_148_916_236 | 2_148_916_237 => Self::AdultVerificationRequired,
            2_148_916_238 => Self::ChildAccount,
            _ => Self::Unknown,
        }
    }

    /// The `ephervex-i18n` message id describing this refusal.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::AccountBanned => "error-auth-xbox-account-banned",
            Self::NoXboxAccount => "error-auth-xbox-no-account",
            Self::CountryUnavailable => "error-auth-xbox-country-unavailable",
            Self::AdultVerificationRequired => "error-auth-xbox-adult-verification",
            Self::ChildAccount => "error-auth-xbox-child-account",
            Self::Unknown => "error-auth-xbox-denied",
        }
    }
}

impl std::fmt::Display for XstsDenial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::AccountBanned => "account banned",
            Self::NoXboxAccount => "no xbox account",
            Self::CountryUnavailable => "country unavailable",
            Self::AdultVerificationRequired => "adult verification required",
            Self::ChildAccount => "child account",
            Self::Unknown => "unspecified",
        })
    }
}

/// Which keyring operation failed. Named because "could not read" and "could
/// not delete" lead to different advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialOperation {
    Store,
    Load,
    Delete,
}

impl std::fmt::Display for CredentialOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Store => "store",
            Self::Load => "load",
            Self::Delete => "delete",
        })
    }
}

impl AuthError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    ///
    /// Several variants choose between two ids on the HTTP status, and that is
    /// the point rather than an accident: "the service is briefly unavailable"
    /// and "this request will be refused identically forever" need different
    /// sentences, and telling a user to wait when waiting cannot help is the
    /// specific failure the project's "actionable next step" exists to prevent.
    /// Transience is decided by `ephervex-download`'s
    /// [`status_is_transient`], so there is one definition of the word in the
    /// workspace rather than a second one drifting here.
    #[must_use]
    pub fn message_id(&self) -> &'static str {
        match self {
            Self::ClientIdMissing => "error-auth-client-id-missing",
            Self::ClientIdInvalid => "error-auth-client-id-invalid",
            Self::EndpointUnusable { .. } => "error-auth-endpoint-unusable",
            Self::ClientBuild { .. } | Self::Transport { .. } => "error-auth-unreachable",
            Self::HttpStatus { status, .. } => {
                if status_is_transient(*status) {
                    "error-auth-service-unavailable"
                } else {
                    "error-auth-unexpected-response"
                }
            }
            Self::MalformedResponse { .. } | Self::ResponseTooLarge { .. } => {
                "error-auth-unexpected-response"
            }
            Self::OAuth { .. } => "error-auth-microsoft-rejected",
            Self::Declined => "error-auth-declined",
            Self::DeviceCodeExpired => "error-auth-device-code-expired",
            Self::TimedOut { .. } => "error-auth-timed-out",
            Self::StateMismatch => "error-auth-state-mismatch",
            Self::RedirectListener { .. } => "error-auth-redirect-listener",
            Self::Canceled => "error-auth-canceled",
            Self::XboxAuthenticationFailed { status } => {
                if status_is_transient(*status) {
                    "error-auth-service-unavailable"
                } else {
                    "error-auth-xbox-authentication-failed"
                }
            }
            // The XSTS reason *is* the message: which of the six it is decides
            // what the user has to go and do.
            Self::XstsDenied { reason, .. } => reason.message_id(),
            Self::XboxUserHashMissing => "error-auth-xbox-user-hash-missing",
            Self::MinecraftLoginRejected { status } => {
                if status_is_transient(*status) {
                    "error-auth-minecraft-unavailable"
                } else {
                    "error-auth-minecraft-login-rejected"
                }
            }
            Self::MinecraftApiNotApproved { .. } => "error-auth-minecraft-api-not-approved",
            Self::GameNotOwned { game_pass, .. } => {
                if *game_pass {
                    "error-auth-game-pass-setup-required"
                } else {
                    "error-auth-game-not-owned"
                }
            }
            Self::GameOwnershipUnconfirmed => "error-auth-game-ownership-unconfirmed",
            Self::ProfileUnavailable { status } => {
                if status_is_transient(*status) {
                    "error-auth-minecraft-unavailable"
                } else {
                    "error-auth-profile-unavailable"
                }
            }
            Self::ProfileMalformed => "error-auth-profile-malformed",
            Self::SecretStorageUnavailable { .. } => "error-auth-secret-storage-unavailable",
            Self::CredentialChangeBusy { .. } => "error-auth-credential-change-busy",
            Self::CredentialChangeUnavailable { .. } => "error-auth-credential-change-unavailable",
            Self::CredentialValueBlank => "error-auth-credential-blank",
            Self::CredentialStore { .. } => "error-auth-credential-store-failed",
            Self::OfflineNameInvalid { .. } => "error-auth-offline-name-invalid",
            Self::AccountUnknown { .. } => "error-auth-account-unknown",
            Self::AccountAmbiguous { .. } => "error-auth-account-ambiguous",
            Self::OfflineAccountExists { .. } => "error-auth-offline-account-exists",
            Self::NotSignedIn => "error-auth-not-signed-in",
            Self::AccountMismatch { .. } => "error-auth-account-mismatch",
            Self::ReauthenticationRequired { .. } => "error-auth-reauthentication-required",
        }
    }

    /// A page the user has to visit to resolve this failure, where one exists.
    ///
    /// Dug out here rather than in the frontend for the same reason as
    /// [`Self::message_id`]: the URL is untranslated technical text that
    /// travels into a message as an argument, and a frontend must
    /// be able to render it - or turn it into a link - without matching on this
    /// enum.
    #[must_use]
    pub const fn help_url(&self) -> Option<&'static str> {
        match self {
            Self::MinecraftApiNotApproved { .. } => Some(MINECRAFT_API_REVIEW_FORM),
            _ => None,
        }
    }

    /// The value the user has to paste into whatever [`Self::help_url`] points
    /// at, where the failure names one.
    #[must_use]
    pub fn help_subject(&self) -> Option<&str> {
        match self {
            Self::MinecraftApiNotApproved { client_id } => Some(client_id),
            _ => None,
        }
    }

    /// Whether this installation cannot authenticate at all until its Azure
    /// application registration is approved.
    ///
    /// Distinct from every other failure in one specific way: no user action
    /// inside this launcher - not signing in again, not waiting, not switching
    /// accounts - can change the outcome. Vanilla launch support's launch gating needs to tell
    /// that apart from "the credential expired", because the two lead to
    /// completely different screens.
    #[must_use]
    pub const fn needs_app_registration_approval(&self) -> bool {
        matches!(self, Self::MinecraftApiNotApproved { .. })
    }

    /// Whether trying the identical operation later could plausibly succeed.
    ///
    /// Deliberately conservative: it is false for everything the launcher
    /// cannot honestly promise will pass on a retry, because "try again in a
    /// few minutes" attached to a permanent condition is worse than no advice
    /// at all.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Transport { .. } | Self::GameOwnershipUnconfirmed => true,
            Self::HttpStatus { status, .. }
            | Self::XboxAuthenticationFailed { status }
            | Self::MinecraftLoginRejected { status }
            | Self::ProfileUnavailable { status } => status_is_transient(*status),
            _ => false,
        }
    }

    /// Whether the only way forward is an interactive sign-in.
    ///
    /// Vanilla launch support turns this into `LaunchError::AuthenticationRequired`: a launch
    /// that fails because a refresh token expired should send the user to the
    /// login button, while one that fails because Xbox Live is down should not
    #[must_use]
    pub const fn requires_interactive_login(&self) -> bool {
        matches!(
            self,
            Self::NotSignedIn
                | Self::AccountMismatch { .. }
                | Self::ReauthenticationRequired { .. }
                | Self::ClientIdMissing
        )
    }

    /// Whether the user asked for this outcome rather than hitting a fault.
    #[must_use]
    pub const fn is_cancellation(&self) -> bool {
        matches!(self, Self::Canceled)
    }

    /// Whether session-only login is a sensible thing to offer next.
    ///
    /// True for exactly one failure. ADR 0004 requires the offer to be explicit
    /// and requires it *not* to be a silent fallback, so the decision to make it
    /// lives here rather than being re-derived by each frontend.
    #[must_use]
    pub const fn session_only_would_help(&self) -> bool {
        matches!(self, Self::SecretStorageUnavailable { .. })
    }

    /// Builds a transport failure for one leg of the chain.
    pub(crate) fn transport(stage: AuthStage, source: reqwest::Error) -> Self {
        Self::Transport {
            stage,
            source: Box::new(source),
        }
    }

    /// Builds a parse failure for one leg of the chain.
    pub(crate) fn malformed(stage: AuthStage, source: serde_json::Error) -> Self {
        Self::MalformedResponse {
            stage,
            source: Box::new(source),
        }
    }
}

/// Shorthand for an authentication result.
pub type AuthResult<T> = Result<T, AuthError>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn every_documented_xerr_decodes_to_its_own_situation() {
        // Microsoft documents six codes and they lead to five different places
        // for the user to go; collapsing them would be the silent approximation
        // Ephervex does not allow.
        assert_eq!(
            XstsDenial::from_xerr(2_148_916_227),
            XstsDenial::AccountBanned
        );
        assert_eq!(
            XstsDenial::from_xerr(2_148_916_233),
            XstsDenial::NoXboxAccount
        );
        assert_eq!(
            XstsDenial::from_xerr(2_148_916_235),
            XstsDenial::CountryUnavailable
        );
        assert_eq!(
            XstsDenial::from_xerr(2_148_916_236),
            XstsDenial::AdultVerificationRequired
        );
        assert_eq!(
            XstsDenial::from_xerr(2_148_916_237),
            XstsDenial::AdultVerificationRequired
        );
        assert_eq!(
            XstsDenial::from_xerr(2_148_916_238),
            XstsDenial::ChildAccount
        );
        assert_eq!(XstsDenial::from_xerr(1), XstsDenial::Unknown);
    }

    #[test]
    fn only_the_failures_a_relogin_fixes_ask_for_one() {
        assert!(AuthError::NotSignedIn.requires_interactive_login());
        assert!(
            AuthError::ReauthenticationRequired {
                code: "invalid_grant".to_owned()
            }
            .requires_interactive_login()
        );
        // Xbox Live being down is not fixed by signing in again, and offering
        // that would send the user round a loop.
        assert!(!AuthError::XboxAuthenticationFailed { status: 503 }.requires_interactive_login());
        assert!(
            !AuthError::GameNotOwned {
                entitlements: Vec::new(),
                game_pass: false,
            }
            .requires_interactive_login()
        );
    }

    #[test]
    fn session_only_is_offered_for_exactly_one_failure() {
        assert!(
            AuthError::SecretStorageUnavailable {
                detail: "no default store".to_owned()
            }
            .session_only_would_help()
        );
        assert!(!AuthError::Declined.session_only_would_help());
    }

    #[test]
    fn owning_the_game_and_owning_game_pass_are_told_apart() {
        let neither = AuthError::GameNotOwned {
            entitlements: Vec::new(),
            game_pass: false,
        };
        let game_pass = AuthError::GameNotOwned {
            entitlements: vec!["product_game_pass_pc".to_owned()],
            game_pass: true,
        };
        assert_ne!(neither.message_id(), game_pass.message_id());
    }

    #[test]
    fn unconfirmed_ownership_is_not_reported_as_not_owning_the_game() {
        let unconfirmed = AuthError::GameOwnershipUnconfirmed;
        assert_ne!(
            unconfirmed.message_id(),
            AuthError::GameNotOwned {
                entitlements: Vec::new(),
                game_pass: false,
            }
            .message_id()
        );
        assert!(unconfirmed.is_transient());
        assert!(!unconfirmed.requires_interactive_login());
    }

    #[test]
    fn no_two_variants_share_a_message_id_by_accident() {
        let ids = [
            AuthError::ClientIdMissing.message_id(),
            AuthError::Declined.message_id(),
            AuthError::DeviceCodeExpired.message_id(),
            AuthError::StateMismatch.message_id(),
            AuthError::Canceled.message_id(),
            AuthError::NotSignedIn.message_id(),
            AuthError::XboxUserHashMissing.message_id(),
            AuthError::ProfileMalformed.message_id(),
            AuthError::GameOwnershipUnconfirmed.message_id(),
        ];
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(sorted.len(), before, "duplicate message id in {ids:?}");
    }
}
