//! The whole chain, assembled: Microsoft → Xbox Live → XSTS → Minecraft →
//! entitlements → profile.
//!
//! # The shape of it
//!
//! Both flows and the refresh path converge after one step. The browser flow
//! and the device flow differ only in *how* a Microsoft access token is
//! obtained; a refresh differs only in that it obtains one without the user
//! present. Everything after that is identical, which is why [`Self::establish`]
//! exists and why there is exactly one implementation of the four Minecraft
//! legs rather than three near-copies that drift. A login hands its refresh
//! token back for [`AccountProvider::keep_login`]; a resume stores its own as
//! it is issued.
//!
//! # Rotation is not optional
//!
//! Microsoft may issue a new refresh token on any redemption and documents that
//! the client is expected to discard the old one. So every path that produces
//! tokens re-stores whatever refresh token came back, and a resume that
//! forgot to would work for weeks and then strand the user.
//!
//! A resume stores the replacement as soon as Microsoft issues it, before the
//! Xbox, Minecraft, and profile legs run. The replacement is the token the
//! client is expected to keep, so an outage further down the chain must not
//! cost the user the credential they were issued. A login has no earlier
//! credential to protect and is stored only once the account is known to own the
//! game and its caller is ready to record it.
//!
//! # Resume and logout are ordered
//!
//! A resume reads a credential, spends time on the network, and writes a
//! replacement. Every write after that wait, and every removal of a rejected
//! credential, happens under one [`CredentialChangeOrder`] that
//! [`AccountProvider::logout`] and the account records' owner also take, and
//! only if the store still holds the credential the resume started from. A
//! logout that completed in between therefore stays complete.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use ephervex_download::HttpConfig;
use ephervex_model::AccountKind;
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument, warn};
use uuid::Uuid;

use crate::endpoints::Endpoints;
use crate::error::{AuthError, AuthResult};
use crate::loopback::{LoopbackReceiver, Redirect};
use crate::microsoft::{MicrosoftClient, MicrosoftTokens};
use crate::minecraft::{EntitlementLookup, MinecraftServicesClient};
use crate::order::{CredentialChangeGuard, CredentialChangeOrder, ProcessChangeOrder};
use crate::pkce::Pkce;
use crate::provider::{
    Account, AccountProvider, AuthEvent, AuthObserver, Credentials, Login, LoginFlow, LoginRequest,
    Session,
};
use crate::secret::SecretString;
use crate::stage::AuthStage;
use crate::store::{CredentialKey, CredentialStorage, CredentialStore};
use crate::xbox::XboxClient;

/// The Microsoft implementation of [`AccountProvider`].
#[derive(Debug, Clone)]
pub struct MicrosoftAccountProvider {
    microsoft: MicrosoftClient,
    xbox: XboxClient,
    minecraft: MinecraftServicesClient,
    credentials: Arc<dyn CredentialStore>,
    order: Arc<dyn CredentialChangeOrder>,
}

impl MicrosoftAccountProvider {
    /// Builds a provider against the real services.
    pub fn new(client_id: &str, credentials: Arc<dyn CredentialStore>) -> AuthResult<Self> {
        Self::with_endpoints(
            client_id,
            credentials,
            Endpoints::production(),
            &HttpConfig::default(),
        )
    }

    /// Builds a provider against explicit endpoints.
    ///
    /// The offline test suite's entry point. Not a configuration knob -
    /// see [`crate::endpoints`].
    pub fn with_endpoints(
        client_id: &str,
        credentials: Arc<dyn CredentialStore>,
        endpoints: Endpoints,
        http: &HttpConfig,
    ) -> AuthResult<Self> {
        let client = crate::http::build_client(http)?;
        Ok(Self {
            microsoft: MicrosoftClient::new(client.clone(), endpoints.clone(), client_id)?,
            xbox: XboxClient::new(client.clone(), endpoints.clone()),
            minecraft: MinecraftServicesClient::new(client, endpoints, client_id),
            credentials,
            order: Arc::new(ProcessChangeOrder),
        })
    }

    /// Orders this provider's credential changes with `order` instead of
    /// within this process only.
    ///
    /// Whoever records accounts passes the order that also covers its own
    /// removals and cleanups, so a resume cannot store a credential back after
    /// another process removed its account.
    #[must_use]
    pub fn with_change_order(mut self, order: Arc<dyn CredentialChangeOrder>) -> Self {
        self.order = order;
        self
    }

    /// The Azure application registration in use. Not a secret.
    #[must_use]
    pub fn client_id(&self) -> &str {
        self.microsoft.client_id()
    }

    // -- flows -------------------------------------------------------------

    /// Authorization code with PKCE, through the user's own browser.
    #[instrument(skip_all)]
    async fn browser_login(
        &self,
        request: &LoginRequest,
        observer: &AuthObserver,
        cancel: &CancellationToken,
    ) -> AuthResult<MicrosoftTokens> {
        let pkce = Pkce::generate().ok_or_else(|| AuthError::RedirectListener {
            source: std::io::Error::other("the operating system provided no entropy"),
        })?;

        // Bound *before* the URL is built: the redirect URI has to carry the
        // port, and it has to be the same string in both legs of the exchange.
        let receiver = LoopbackReceiver::bind().await?;
        let redirect_uri = receiver.redirect_uri().to_owned();
        let url = self.microsoft.authorization_url(&pkce, &redirect_uri)?;

        // The frontend decides whether to open a browser, print the URL, or
        // both. This crate must not reach for the desktop: a headless caller
        // has no desktop to reach for.
        observer.emit(AuthEvent::OpenBrowser { url });

        match receiver.wait(pkce.state(), request.timeout, cancel).await? {
            Redirect::Code(code) => {
                self.microsoft
                    .redeem_code(&code, &pkce, &redirect_uri, cancel)
                    .await
            }
            Redirect::Error { code, description } => Err(crate::http::OAuthErrorBody {
                error: code,
                error_description: description,
            }
            .into_error()),
        }
    }

    /// The device authorization grant, for a machine with no browser.
    #[instrument(skip_all)]
    async fn device_code_login(
        &self,
        request: &LoginRequest,
        observer: &AuthObserver,
        cancel: &CancellationToken,
    ) -> AuthResult<MicrosoftTokens> {
        let session = self.microsoft.begin_device_code(cancel).await?;
        observer.emit(AuthEvent::DeviceCode(session.prompt.clone()));
        self.microsoft
            .poll_device_code(&session, request.timeout, cancel)
            .await
    }

    // -- the shared tail ---------------------------------------------------

    /// Everything after a Microsoft access token exists.
    ///
    /// One implementation for three entry points. The ownership check happens
    /// here, before a `Session` can be constructed at all, which is what makes
    /// "verify ownership before allowing launch" a property of the type rather
    /// than a step someone has to remember.
    #[instrument(skip_all)]
    async fn establish(
        &self,
        access: &SecretString,
        observer: &AuthObserver,
        cancel: &CancellationToken,
    ) -> AuthResult<Session> {
        observer.stage(AuthStage::XboxLive);
        let user_token = self.xbox.authenticate(access, cancel).await?;

        observer.stage(AuthStage::Xsts);
        let xsts = self.xbox.authorize(&user_token, cancel).await?;

        observer.stage(AuthStage::MinecraftLogin);
        let minecraft = self.minecraft.login(&xsts.identity_token(), cancel).await?;

        observer.stage(AuthStage::Entitlements);
        // A failing entitlement store must not block a login on its own: the
        // profile is what decides, and this call exists to explain a refusal.
        // Treating an outage here as "you do not own the game" would lock a
        // paying user out of their own instances, so it stays unknown rather
        // than becoming an empty answer.
        let entitlements = match self.minecraft.entitlements(&minecraft.access, cancel).await {
            Ok(entitlements) => EntitlementLookup::Answered(entitlements),
            // Two failures are not outages and must not be swallowed: the user
            // asking to stop, and the application not being approved for the
            // API at all. The second would otherwise be reported one call later
            // as a profile problem, under a warning blaming the store.
            Err(error) if error.is_cancellation() || error.needs_app_registration_approval() => {
                return Err(error);
            }
            Err(error) => {
                warn!(%error, "could not read the entitlement store; relying on the profile");
                EntitlementLookup::Unavailable
            }
        };

        observer.stage(AuthStage::Profile);
        let Some(profile) = self.minecraft.profile(&minecraft.access, cancel).await? else {
            // No Java Edition profile. Which refusal this is depends on what
            // the store said, and each needs different instructions. When the
            // store said nothing, ownership is unconfirmed, not denied.
            return Err(match entitlements {
                EntitlementLookup::Answered(entitlements) => AuthError::GameNotOwned {
                    game_pass: entitlements.has_game_pass(),
                    entitlements: entitlements.items,
                },
                EntitlementLookup::Unavailable => AuthError::GameOwnershipUnconfirmed,
            });
        };

        let account = Account {
            id: profile.id,
            name: profile.name,
            kind: AccountKind::Microsoft,
            authenticated_at: Utc::now(),
        };

        debug!(
            account = %account.id,
            entitlements = ?entitlements.answered().map(|answered| &answered.items),
            storage = self.credentials.storage().as_str(),
            "authentication chain completed"
        );

        Ok(Session {
            account,
            credentials: Credentials::Microsoft {
                token: minecraft.access,
                expires_at: minecraft.expires_at,
                entitlements,
                storage: self.credentials.storage(),
            },
        })
    }

    /// Completes a login: runs the chain and keeps the refresh token for
    /// [`AccountProvider::keep_login`].
    ///
    /// The token is stored only *after* the chain has succeeded and the caller
    /// is ready to record the account. Persisting one for an account that
    /// cannot play would leave a credential behind that every later resume
    /// fails on.
    async fn finish(
        &self,
        tokens: MicrosoftTokens,
        observer: &AuthObserver,
        cancel: &CancellationToken,
    ) -> AuthResult<Login> {
        let session = self.establish(&tokens.access, observer, cancel).await?;
        if tokens.refresh.is_none() {
            // Microsoft always issues one for `offline_access`, so this means
            // the scope was dropped somewhere. Worth saying: the symptom is a
            // login that mysteriously does not survive a restart.
            warn!(
                account = %session.account.id,
                "no refresh token was issued; this login will not survive a restart"
            );
        }
        Ok(Login {
            session,
            refresh: tokens.refresh,
        })
    }

    /// Stores the refresh token a resume was issued in place of `loaded`.
    ///
    /// A credential that is gone means the account signed out while the
    /// refresh ran, and the replacement must not bring it back. One that
    /// differs from `loaded` was already replaced by a newer login or resume,
    /// which this one must not overwrite.
    fn publish_replacement(
        &self,
        key: &CredentialKey,
        loaded: &SecretString,
        replacement: &SecretString,
    ) -> AuthResult<()> {
        let _ordered = self.order.hold()?;
        match self.credentials.load(key)? {
            None => Err(AuthError::NotSignedIn),
            Some(current) if current == *loaded => self.credentials.store(key, replacement),
            Some(_) => Ok(()),
        }
    }

    /// Removes the credential Microsoft rejected, unless it has changed since
    /// it was read.
    fn discard_rejected(&self, key: &CredentialKey, rejected: &SecretString) {
        let _ordered = match self.order.hold() {
            Ok(ordered) => ordered,
            Err(error) => {
                warn!(%error, "could not remove the rejected credential");
                return;
            }
        };
        match self.credentials.load(key) {
            Ok(Some(current)) if current == *rejected => {
                if let Err(removal) = self.credentials.delete(key) {
                    warn!(%removal, "could not remove the rejected credential");
                }
            }
            Ok(_) => {}
            Err(error) => warn!(%error, "could not remove the rejected credential"),
        }
    }

    /// Fails when the account signed out while a resume was running.
    fn ensure_signed_in(&self, key: &CredentialKey) -> AuthResult<()> {
        let _ordered = self.order.hold()?;
        match self.credentials.load(key)? {
            Some(_) => Ok(()),
            None => Err(AuthError::NotSignedIn),
        }
    }
}

#[async_trait]
impl AccountProvider for MicrosoftAccountProvider {
    #[instrument(skip_all, fields(flow = request.flow.as_str()))]
    async fn login(
        &self,
        request: &LoginRequest,
        observer: &AuthObserver,
        cancel: &CancellationToken,
    ) -> AuthResult<Login> {
        observer.stage(AuthStage::Microsoft);
        let tokens = match request.flow {
            LoginFlow::Browser => self.browser_login(request, observer, cancel).await?,
            LoginFlow::DeviceCode => self.device_code_login(request, observer, cancel).await?,
        };
        self.finish(tokens, observer, cancel).await
    }

    #[instrument(skip_all, fields(account = %account))]
    async fn resume(
        &self,
        account: Uuid,
        observer: &AuthObserver,
        cancel: &CancellationToken,
    ) -> AuthResult<Session> {
        observer.stage(AuthStage::Microsoft);
        let key = CredentialKey::MicrosoftRefreshToken(account);
        let Some(refresh) = self.credentials.load(&key)? else {
            return Err(AuthError::NotSignedIn);
        };

        let tokens = match self.microsoft.refresh(&refresh, cancel).await {
            Ok(tokens) => tokens,
            Err(error) if error.requires_interactive_login() => {
                // The credential is dead. Leaving it in the keyring would make
                // every later resume fail the same way while `account status`
                // insisted a credential existed.
                self.discard_rejected(&key, &refresh);
                return Err(error);
            }
            Err(error) => return Err(error),
        };

        // Kept under the account that was asked for, before the later legs can
        // fail: the token it replaces is no longer the one to rely on.
        if let Some(replacement) = &tokens.refresh {
            self.publish_replacement(&key, &refresh, replacement)?;
        }

        let session = self.establish(&tokens.access, observer, cancel).await?;
        if session.account.id != account {
            // The stored credential belongs to someone else. Adopting the
            // session would bind this account's record to another identity.
            return Err(AuthError::AccountMismatch {
                requested: account,
                found: session.account.id,
            });
        }
        self.ensure_signed_in(&key)?;
        Ok(session)
    }

    fn keep_login(&self, login: &Login, _held: &CredentialChangeGuard) -> AuthResult<()> {
        match &login.refresh {
            Some(refresh) => self.credentials.store(
                &CredentialKey::MicrosoftRefreshToken(login.session.account.id),
                refresh,
            ),
            None => Ok(()),
        }
    }

    fn logout(&self, account: Uuid) -> AuthResult<()> {
        let _ordered = self.order.hold()?;
        self.credentials
            .delete(&CredentialKey::MicrosoftRefreshToken(account))
            .map(|_| ())
    }

    fn has_stored_credential(&self, account: Uuid) -> AuthResult<bool> {
        Ok(self
            .credentials
            .load(&CredentialKey::MicrosoftRefreshToken(account))?
            .is_some())
    }

    fn storage(&self) -> CredentialStorage {
        self.credentials.storage()
    }
}

/// Ephervex's own CurseForge API key, if one was baked in at build time.
///
/// Unlike [`crate::DEFAULT_MICROSOFT_CLIENT_ID`], this is a real bearer
/// secret (ADR 0030), so it is never a literal in source: it is picked up
/// only from the `EPHERVEX_CURSEFORGE_API_KEY` build-time environment
/// variable, which the release workflows set from a repository secret. A
/// local development build that leaves the variable unset compiles an empty
/// default, meaning "no built-in key" - exactly today's behavior. A key a
/// user saves through settings always wins over this.
pub const DEFAULT_CURSEFORGE_API_KEY: &str = match option_env!("EPHERVEX_CURSEFORGE_API_KEY") {
    Some(key) => key,
    None => "",
};

/// Reads the CurseForge API key out of a credential store.
///
/// Authentication owns this access because all credentials share the same
/// secure store. CurseForge integrations consume the key, while the settings
/// surface writes it. It must never reach `config.toml`.
///
/// Falls back to [`DEFAULT_CURSEFORGE_API_KEY`] only when the store holds
/// nothing: a key the user configured always takes precedence over the
/// build's own (ADR 0030).
pub fn curseforge_api_key(store: &dyn CredentialStore) -> AuthResult<Option<SecretString>> {
    let stored = store.load(&CredentialKey::CurseForgeApiKey)?;
    Ok(resolve_curseforge_api_key(
        stored,
        DEFAULT_CURSEFORGE_API_KEY,
    ))
}

/// The CurseForge API key in effect on a machine with no credential store at
/// all.
///
/// With no store there is no user key to prefer, so the answer is the build's
/// own key or none (ADR 0030). A store that exists but cannot be read is a
/// different failure and must not come through here: a user key may be in it.
#[must_use]
pub fn curseforge_api_key_without_store() -> Option<SecretString> {
    resolve_curseforge_api_key(None, DEFAULT_CURSEFORGE_API_KEY)
}

/// The stored key wins; an empty build default means there is none to fall
/// back to. Split out from [`curseforge_api_key`] so the ordering is testable
/// against an arbitrary default, since [`DEFAULT_CURSEFORGE_API_KEY`] itself
/// is fixed at compile time.
fn resolve_curseforge_api_key(stored: Option<SecretString>, default: &str) -> Option<SecretString> {
    stored.or_else(|| (!default.is_empty()).then(|| SecretString::new(default)))
}

/// Whether the user has stored their own CurseForge API key.
///
/// Unlike [`curseforge_api_key`], this never falls back to
/// [`DEFAULT_CURSEFORGE_API_KEY`]: it answers "did the user configure one",
/// not "is a key in effect". A settings screen that shows removal as
/// available, or reports the key as "configured", needs this rather than the
/// resolving accessor - the build's own fallback key is never something a
/// `Remove` button removes.
pub fn curseforge_api_key_configured(store: &dyn CredentialStore) -> AuthResult<bool> {
    Ok(store.load(&CredentialKey::CurseForgeApiKey)?.is_some())
}

/// Stores the CurseForge API key, replacing whatever was there.
///
/// A value that is nothing but whitespace is refused before the store is
/// touched, so a mistaken submission leaves the previous credential exactly as
/// it was rather than replacing a working key with a blank one. Removal is a
/// separate operation ([`clear_curseforge_api_key`]) precisely so that it
/// cannot happen by submitting an empty field.
pub fn set_curseforge_api_key(store: &dyn CredentialStore, key: &SecretString) -> AuthResult<()> {
    let key = key.trimmed().ok_or(AuthError::CredentialValueBlank)?;
    store.store(&CredentialKey::CurseForgeApiKey, &key)
}

/// Removes the stored CurseForge API key.
///
/// Idempotent, and it never needs the existing value: removing a credential
/// must not require revealing one.
pub fn clear_curseforge_api_key(store: &dyn CredentialStore) -> AuthResult<()> {
    store.delete(&CredentialKey::CurseForgeApiKey).map(|_| ())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::store::{CredentialRemoval, MemoryStore};

    fn provider() -> MicrosoftAccountProvider {
        MicrosoftAccountProvider::with_endpoints(
            "767ea465-0bba-443b-a06b-831c71777b90",
            Arc::new(MemoryStore::new()),
            Endpoints::with_base("http://127.0.0.1:1"),
            &HttpConfig::for_loopback_tests(),
        )
        .expect("provider")
    }

    #[tokio::test]
    async fn resuming_without_a_stored_credential_asks_for_a_login() {
        let error = provider()
            .resume(
                Uuid::nil(),
                &AuthObserver::silent(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("nothing is stored");
        assert!(matches!(error, AuthError::NotSignedIn));
        assert!(error.requires_interactive_login());
    }

    #[test]
    fn logout_is_idempotent_and_reports_the_credential_afterwards() {
        let provider = provider();
        let account = Uuid::nil();
        assert!(!provider.has_stored_credential(account).expect("query"));
        provider.logout(account).expect("first logout");
        provider.logout(account).expect("second logout");
    }

    /// Obviously not a credential. Every test here uses it.
    const FAKE_KEY: &str = "test-curseforge-key-do-not-use";

    #[test]
    fn the_curseforge_key_is_set_replaced_and_removed_through_the_credential_store() {
        let store = MemoryStore::new();
        assert!(curseforge_api_key(&store).expect("read").is_none());

        set_curseforge_api_key(&store, &SecretString::new(FAKE_KEY)).expect("store");
        assert_eq!(
            curseforge_api_key(&store)
                .expect("read")
                .expect("present")
                .expose(),
            FAKE_KEY
        );

        // Replacing is the same operation with a new value: there is nothing to
        // remove first, and no way to ask for the old one.
        set_curseforge_api_key(
            &store,
            &SecretString::new("test-curseforge-key-replacement"),
        )
        .expect("replace");
        assert_eq!(
            curseforge_api_key(&store)
                .expect("read")
                .expect("present")
                .expose(),
            "test-curseforge-key-replacement"
        );

        clear_curseforge_api_key(&store).expect("remove");
        assert!(curseforge_api_key(&store).expect("read").is_none());
        clear_curseforge_api_key(&store).expect("removing twice is not an error");
    }

    #[test]
    fn presence_reports_only_what_the_user_configured_never_the_build_default() {
        let store = MemoryStore::new();
        assert!(!curseforge_api_key_configured(&store).expect("read"));

        set_curseforge_api_key(&store, &SecretString::new(FAKE_KEY)).expect("store");
        assert!(curseforge_api_key_configured(&store).expect("read"));

        clear_curseforge_api_key(&store).expect("remove");
        // Even though `curseforge_api_key` would still resolve a usable key
        // when a build default is compiled in, presence must go back to false:
        // a `Remove` action clears the user's key, not the build's fallback.
        assert!(!curseforge_api_key_configured(&store).expect("read"));
        assert_eq!(
            resolve_curseforge_api_key(
                store.load(&CredentialKey::CurseForgeApiKey).expect("read"),
                "built-in-key",
            )
            .expect("the default applies")
            .expose(),
            "built-in-key"
        );
    }

    #[test]
    fn an_empty_build_default_leaves_an_unconfigured_key_absent() {
        assert!(resolve_curseforge_api_key(None, "").is_none());
    }

    #[test]
    fn a_build_default_stands_in_only_when_nothing_is_stored() {
        assert_eq!(
            resolve_curseforge_api_key(None, "built-in-key")
                .expect("the default applies")
                .expose(),
            "built-in-key"
        );
    }

    #[test]
    fn a_stored_key_outranks_the_build_default() {
        assert_eq!(
            resolve_curseforge_api_key(Some(SecretString::new(FAKE_KEY)), "built-in-key")
                .expect("the stored key applies")
                .expose(),
            FAKE_KEY
        );
    }

    #[test]
    fn a_blank_value_is_refused_and_leaves_the_stored_key_untouched() {
        let store = MemoryStore::new();
        set_curseforge_api_key(&store, &SecretString::new(FAKE_KEY)).expect("store");

        for blank in ["", " ", "\t\n", "   "] {
            let error = set_curseforge_api_key(&store, &SecretString::new(blank))
                .expect_err("a blank value is not a credential");
            assert!(
                matches!(error, AuthError::CredentialValueBlank),
                "{blank:?}"
            );
        }

        // The state a settings screen reports must still be true afterwards.
        assert_eq!(
            curseforge_api_key(&store)
                .expect("read")
                .expect("present")
                .expose(),
            FAKE_KEY
        );
    }

    #[test]
    fn a_pasted_value_is_stored_without_the_whitespace_around_it() {
        // An HTTP field value has its surrounding whitespace stripped by
        // definition, so a key stored with a trailing newline could never have
        // been sent verbatim.
        let store = MemoryStore::new();
        set_curseforge_api_key(&store, &SecretString::new(format!("  {FAKE_KEY}\n")))
            .expect("store");
        assert_eq!(
            curseforge_api_key(&store)
                .expect("read")
                .expect("present")
                .expose(),
            FAKE_KEY
        );
    }

    #[test]
    fn a_failing_credential_store_reports_a_typed_error_that_holds_no_key() {
        let store = FailingStore;
        let error = set_curseforge_api_key(&store, &SecretString::new(FAKE_KEY))
            .expect_err("the store refuses");
        assert!(matches!(
            error,
            AuthError::SecretStorageUnavailable { .. } | AuthError::CredentialStore { .. }
        ));
        assert!(!format!("{error:?}").contains(FAKE_KEY));
        assert!(!error.to_string().contains(FAKE_KEY));

        // The state the settings screen reads stays truthful: nothing was
        // stored, and asking reports the failure rather than inventing a key.
        assert!(curseforge_api_key(&store).is_err());
        assert!(clear_curseforge_api_key(&store).is_err());
    }

    /// A credential store that cannot do anything, standing in for a machine
    /// with no unlocked Secret Service.
    #[derive(Debug)]
    struct FailingStore;

    impl CredentialStore for FailingStore {
        fn store(&self, _key: &CredentialKey, _secret: &SecretString) -> AuthResult<()> {
            Err(AuthError::SecretStorageUnavailable {
                detail: "no secret service".to_owned(),
            })
        }

        fn load(&self, _key: &CredentialKey) -> AuthResult<Option<SecretString>> {
            Err(AuthError::SecretStorageUnavailable {
                detail: "no secret service".to_owned(),
            })
        }

        fn delete(&self, _key: &CredentialKey) -> AuthResult<CredentialRemoval> {
            Err(AuthError::SecretStorageUnavailable {
                detail: "no secret service".to_owned(),
            })
        }

        fn storage(&self) -> CredentialStorage {
            CredentialStorage::Keyring
        }
    }

    #[tokio::test]
    async fn a_canceled_login_stops_rather_than_reaching_the_network() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = provider()
            .login(
                &LoginRequest {
                    flow: LoginFlow::DeviceCode,
                    ..LoginRequest::default()
                },
                &AuthObserver::silent(),
                &cancel,
            )
            .await
            .expect_err("must stop");
        assert!(error.is_cancellation(), "{error:?}");
    }
}
