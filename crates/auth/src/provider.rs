//! The `AccountProvider` interface (ADR 0005), the values that
//! cross it, and the event surface a frontend drives it through.
//!
//! # Why there is an interface at all
//!
//! ADR 0005 puts authentication behind a trait for the same reason it does
//! loaders and mod platforms: the Microsoft/Xbox/Minecraft chain has changed
//! before, is documented in no single stable public specification, and is
//! certain to change again. `ephervex-core` therefore depends on this trait and
//! never on [`crate::chain::MicrosoftAccountProvider`], so replacing the chain
//! is an addition rather than a rewrite of the launch pipeline.
//!
//! # Why a login is an event stream and not a function that blocks
//!
//! Both flows have a step in the middle where the *user* has to go and do
//! something, and the launcher has to say what. The browser flow needs a URL
//! opened; the device flow needs a code and an address displayed. Neither can
//! be a return value, because they arrive before the operation finishes.
//!
//! [`AuthObserver`] is the seam. It is deliberately shaped like
//! `ephervex-download`'s `OperationReporter`: a channel a frontend drains, plus
//! a `silent()` constructor for callers that do not care. The CLI and GUI can
//! subscribe without the authentication crate knowing which frontend is
//! listening or blocking on a window.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ephervex_model::AccountKind;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::error::AuthResult;
use crate::microsoft::DeviceCodePrompt;
use crate::minecraft::Entitlements;
use crate::secret::SecretString;
use crate::stage::AuthStage;
use crate::store::CredentialStorage;

/// Which login flow to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoginFlow {
    /// Authorization code with PKCE and a loopback redirect. The one a GUI
    /// button drives, and the default because it is the flow that needs no
    /// typing.
    #[default]
    Browser,
    /// The device authorization grant: show a code, poll. For a headless
    /// machine, an SSH session, or a CLI on a box with no browser.
    DeviceCode,
}

impl LoginFlow {
    /// A stable identifier for logs and `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::DeviceCode => "device-code",
        }
    }
}

/// What a caller is asking for when it starts a login.
#[derive(Debug, Clone)]
pub struct LoginRequest {
    pub flow: LoginFlow,
    /// How long to wait for the user to finish.
    pub timeout: std::time::Duration,
}

impl Default for LoginRequest {
    fn default() -> Self {
        Self {
            flow: LoginFlow::default(),
            timeout: crate::loopback::DEFAULT_LOGIN_TIMEOUT,
        }
    }
}

/// A player identity, reduced to what anything outside this crate needs.
///
/// Everything here is non-secret and may be written to `config.toml`: the
/// profile id, the display name, the kind, and when the account was last used
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: Uuid,
    pub name: String,
    /// Microsoft or offline. Carried on the account rather than inferred from
    /// whether a token happens to be present, so nothing can lose it.
    pub kind: AccountKind,
    /// When this account last completed the chain, or was created if offline.
    pub authenticated_at: DateTime<Utc>,
}

impl Account {
    /// A local identity with no credential.
    #[must_use]
    pub fn offline(id: Uuid, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            kind: AccountKind::Offline,
            authenticated_at: Utc::now(),
        }
    }
}

/// What a session can prove.
///
/// # Why this is an enum rather than an optional token
///
/// An offline session is not a Microsoft session with the token missing, and
/// modeling it that way would make "no token" mean two different things -
/// "offline" and "expired" - which the launch pipeline must never confuse.
/// Vanilla launch support has to build a command line for both, and the two differ in more
/// than one argument, so it is made to match rather than allowed to unwrap.
///
/// `#[non_exhaustive]` for the same reason: a future third kind must not
/// silently fall into whichever arm was written with a wildcard.
#[derive(Debug)]
#[non_exhaustive]
pub enum Credentials {
    /// A real Minecraft session.
    ///
    /// The access token lives here and only here - in memory, for the life of
    /// the process. Persist it only when unavoidable; it is
    /// not: it expires in twenty-four hours and the refresh token can mint
    /// another, so writing it anywhere would add a second credential at risk
    /// for no benefit.
    Microsoft {
        token: SecretString,
        expires_at: DateTime<Utc>,
        /// What the account is entitled to. Kept so a frontend can say *why* a
        /// launch is allowed, and so a Game Pass account is recognizable.
        entitlements: Entitlements,
        /// Where the refresh token was put. `SessionOnly` means this login does
        /// not survive a restart, and the user was told so before it started.
        storage: CredentialStorage,
    },
    /// There is no credential and there never will be one.
    ///
    /// Not a degraded Microsoft session. An offline identity proves nothing,
    /// which is exactly why it may not be offered to an online-mode server.
    Offline,
}

/// An account that is ready to be launched with.
#[derive(Debug)]
pub struct Session {
    pub account: Account,
    pub credentials: Credentials,
}

impl Session {
    /// A session for a local identity, available immediately and forever.
    #[must_use]
    pub const fn offline(account: Account) -> Self {
        Self {
            account,
            credentials: Credentials::Offline,
        }
    }

    /// Whether this session can be launched with right now.
    ///
    /// The margin matters for a Microsoft session: a token that expires in four
    /// seconds passes a naive check and then fails part-way through a launch
    /// that has already started downloading assets. An offline session has
    /// nothing to expire.
    #[must_use]
    pub fn is_usable(&self, margin: chrono::TimeDelta) -> bool {
        match &self.credentials {
            Credentials::Microsoft { expires_at, .. } => *expires_at > Utc::now() + margin,
            Credentials::Offline => true,
        }
    }

    /// Whether this identity was proved to Mojang.
    ///
    /// The one question an online-mode check should ask. Deliberately derived
    /// from [`Credentials`] rather than from [`Account::kind`], so a session
    /// built with mismatched halves cannot claim more than it holds.
    #[must_use]
    pub const fn is_authenticated(&self) -> bool {
        matches!(self.credentials, Credentials::Microsoft { .. })
    }

    /// The Minecraft access token, for the sessions that have one.
    ///
    /// Returns `None` for an offline session rather than an empty string or a
    /// placeholder: a caller that ignores the `None` gets a compile error or a
    /// visible panic in tests, not a launch that quietly sends `""` as a token.
    #[must_use]
    pub const fn access_token(&self) -> Option<&SecretString> {
        match &self.credentials {
            Credentials::Microsoft { token, .. } => Some(token),
            Credentials::Offline => None,
        }
    }

    /// When this session stops working, for the sessions that expire.
    #[must_use]
    pub const fn expires_at(&self) -> Option<DateTime<Utc>> {
        match &self.credentials {
            Credentials::Microsoft { expires_at, .. } => Some(*expires_at),
            Credentials::Offline => None,
        }
    }

    /// What the account is entitled to. Empty for an offline account, which is
    /// entitled to nothing and needs to be.
    #[must_use]
    pub fn entitlements(&self) -> Option<&Entitlements> {
        match &self.credentials {
            Credentials::Microsoft { entitlements, .. } => Some(entitlements),
            Credentials::Offline => None,
        }
    }

    /// Where this session's refresh credential was stored, if it has one.
    #[must_use]
    pub const fn storage(&self) -> Option<CredentialStorage> {
        match &self.credentials {
            Credentials::Microsoft { storage, .. } => Some(*storage),
            Credentials::Offline => None,
        }
    }
}

/// Something a login wants to tell the user while it is running.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AuthEvent {
    /// Open this URL in a browser. The browser flow's one interactive step.
    ///
    /// The URL is not a secret - it is a public authorization request - but it
    /// carries the `state` value, so it is emitted rather than logged.
    OpenBrowser { url: String },
    /// Show this code and address. The device flow's one interactive step.
    DeviceCode(DeviceCodePrompt),
    /// The chain moved on to another service.
    Stage(AuthStage),
}

/// Where [`AuthEvent`]s go.
///
/// Cloneable and cheap, so the chain can hand one to each leg. A dropped
/// receiver is not an error: a caller that stopped listening still wants the
/// login it started to finish.
#[derive(Debug, Clone, Default)]
pub struct AuthObserver {
    sender: Option<mpsc::UnboundedSender<AuthEvent>>,
}

impl AuthObserver {
    /// Creates an observer and the receiver a frontend should drain.
    ///
    /// Unbounded because a slow consumer must never be able to stall a login,
    /// and a whole flow emits fewer than ten events.
    #[must_use]
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<AuthEvent>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                sender: Some(sender),
            },
            receiver,
        )
    }

    /// An observer that discards everything, for callers that only want the
    /// result - a refresh before launch, or a test.
    #[must_use]
    pub const fn silent() -> Self {
        Self { sender: None }
    }

    /// Emits an event, ignoring a receiver that has gone away.
    pub fn emit(&self, event: AuthEvent) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(event);
        }
    }

    /// Emits a stage transition.
    pub fn stage(&self, stage: AuthStage) {
        self.emit(AuthEvent::Stage(stage));
    }
}

/// The whole Minecraft-specific authentication chain, behind one interface.
///
/// `async_trait` rather than a native `async fn`: the latter is not
/// dyn-compatible, and `ephervex-core` holds this as `dyn AccountProvider`
/// precisely so the services layer names the interface and not the
/// implementation (ADR 0005).
#[async_trait]
pub trait AccountProvider: std::fmt::Debug + Send + Sync {
    /// Runs an interactive login and, on success, persists the refresh
    /// credential wherever this provider was told to put it.
    async fn login(
        &self,
        request: &LoginRequest,
        observer: &AuthObserver,
        cancel: &CancellationToken,
    ) -> AuthResult<Session>;

    /// Re-establishes a session for an account that has already logged in,
    /// using the stored refresh credential.
    ///
    /// This is both "app restart preserves login" and "refresh before
    /// launch": one operation, because they are the same operation.
    async fn resume(
        &self,
        account: Uuid,
        observer: &AuthObserver,
        cancel: &CancellationToken,
    ) -> AuthResult<Session>;

    /// Removes the stored credential.
    ///
    /// Deletion, not forgetting: ADR 0004 requires logout to take the secret
    /// out of the keyring rather than merely drop it from memory.
    fn logout(&self, account: Uuid) -> AuthResult<()>;

    /// Whether this account has a credential that would survive a restart.
    fn has_stored_credential(&self, account: Uuid) -> AuthResult<bool>;

    /// Where this provider keeps credentials.
    fn storage(&self) -> CredentialStorage;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_default_flow_is_the_one_a_button_drives() {
        // Browser-based PKCE is preferred where practical; device code
        // is the fallback for headless machines, not the norm.
        assert_eq!(LoginFlow::default(), LoginFlow::Browser);
        assert_eq!(LoginRequest::default().flow, LoginFlow::Browser);
    }

    #[test]
    fn a_silent_observer_swallows_events_without_failing() {
        let observer = AuthObserver::silent();
        observer.stage(AuthStage::Microsoft);
        observer.emit(AuthEvent::OpenBrowser {
            url: "https://example.invalid".to_owned(),
        });
    }

    #[test]
    fn a_channel_observer_delivers_in_order() {
        let (observer, mut receiver) = AuthObserver::channel();
        observer.stage(AuthStage::Microsoft);
        observer.stage(AuthStage::Profile);

        assert!(matches!(
            receiver.try_recv().expect("first"),
            AuthEvent::Stage(AuthStage::Microsoft)
        ));
        assert!(matches!(
            receiver.try_recv().expect("second"),
            AuthEvent::Stage(AuthStage::Profile)
        ));
    }

    #[test]
    fn a_dropped_receiver_does_not_break_a_login_in_progress() {
        let (observer, receiver) = AuthObserver::channel();
        drop(receiver);
        // A frontend that closed its window must not take the login with it.
        observer.stage(AuthStage::Xsts);
    }

    fn microsoft_session(expires_in: i64, token: &str) -> Session {
        Session {
            account: Account {
                id: Uuid::nil(),
                name: "Notch".to_owned(),
                kind: AccountKind::Microsoft,
                authenticated_at: Utc::now(),
            },
            credentials: Credentials::Microsoft {
                token: SecretString::new(token),
                expires_at: Utc::now() + chrono::TimeDelta::seconds(expires_in),
                entitlements: Entitlements::default(),
                storage: CredentialStorage::SessionOnly,
            },
        }
    }

    #[test]
    fn a_session_close_to_expiry_is_not_considered_usable() {
        let session = microsoft_session(10, "token");
        assert!(session.is_usable(chrono::TimeDelta::seconds(5)));
        // A launch takes longer than thirty seconds to get as far as needing
        // the token, so a token that expires inside that window is no good.
        assert!(!session.is_usable(chrono::TimeDelta::seconds(60)));
    }

    #[test]
    fn a_session_never_renders_its_token() {
        let rendered = format!("{:?}", microsoft_session(60, "eyJhbGciOi.SECRET"));
        assert!(!rendered.contains("SECRET"), "{rendered}");
        assert!(rendered.contains("Notch"), "non-secret fields still render");
    }

    #[test]
    fn an_offline_session_is_never_authenticated_and_never_expires() {
        // The property the `Credentials` enum exists to guarantee. An offline
        // identity proves nothing, so it must not be offerable to an
        // online-mode server however long it has been around.
        let session = Session::offline(Account::offline(
            crate::offline::offline_uuid("Notch"),
            "Notch",
        ));

        assert!(!session.is_authenticated());
        assert!(
            session.access_token().is_none(),
            "there is no token to hand out"
        );
        assert!(session.expires_at().is_none());
        assert!(session.entitlements().is_none());
        assert!(session.storage().is_none(), "nothing was stored anywhere");

        // It is usable for launching, immediately and indefinitely - that is
        // the whole point of it.
        assert!(session.is_usable(chrono::TimeDelta::days(3650)));
    }

    #[test]
    fn authentication_is_read_from_the_credentials_not_from_the_label() {
        // A session whose account claims Microsoft but which holds no
        // credential must not report itself as authenticated. The two halves
        // are set together in practice; this pins which one wins if they ever
        // disagree.
        let mismatched = Session {
            account: Account {
                id: Uuid::nil(),
                name: "Notch".to_owned(),
                kind: AccountKind::Microsoft,
                authenticated_at: Utc::now(),
            },
            credentials: Credentials::Offline,
        };
        assert!(!mismatched.is_authenticated());
        assert!(mismatched.access_token().is_none());
    }
}
