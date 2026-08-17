//! The account services both frontends call.
//!
//! # What this layer adds over `ephervex-auth`
//!
//! The auth crate knows how to run the token chain. It deliberately knows
//! nothing about configuration files, keyrings-versus-settings, or which
//! account is "the" account — those are application questions, and answering
//! them here is what keeps the provider replaceable (ADR 0005).
//!
//! Concretely this module owns four things:
//!
//! * **Resolving the Client ID.** The user's setting, or the build's default.
//! * **Choosing a credential store.** Keyring when Secret Service answers;
//!   session-only when the caller explicitly asked for it after being told.
//!   Never a file, because there is no file-backed store to pick.
//! * **Remembering the non-secret half.** The profile id, display name, and
//!   timestamps go to `config.toml`; the refresh token went to the keyring
//!   before this module saw it.
//! * **Holding the live session.** In memory, for the life of the process. The
//!   Minecraft access token is never written anywhere.
//!
//! # Why `&mut self`
//!
//! Logging in changes persistent state — the active account, its name, when it
//! last authenticated — so these take `&mut self` while the read-only services
//! elsewhere in [`crate::App`] take `&self`. That is the honest signature for
//! an operation that writes the configuration file, and it costs nothing today:
//! the CLI owns its `App` outright, and the GUI owns one inside
//! its worker rather than sharing it across repaints.

use std::sync::Arc;

use chrono::{SecondsFormat, TimeDelta, Utc};
use ephervex_auth::{
    AccountProvider, AuthError, AuthObserver, CredentialStorage, CredentialStore, KeyringStore,
    LoginRequest, MemoryStore, MicrosoftAccountProvider, SecretString, Session,
};
use ephervex_download::HttpConfig;
use ephervex_model::AccountKind;
use ephervex_storage::AccountLookup;
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument, warn};
use uuid::Uuid;

use crate::App;
use crate::error::{CoreError, CoreResult};

/// How much life a Minecraft token needs left on it to be worth reusing.
///
/// A launch takes tens of seconds to reach the point of needing the token, and
/// a token that expires part-way through produces a disconnect that looks like
/// a server problem. Refreshing early is cheap; refreshing late is not.
pub const TOKEN_REFRESH_MARGIN: TimeDelta = TimeDelta::minutes(5);

/// User-configurable external API settings.
///
/// The CurseForge key is reported as *present or absent*, never as a value:
/// this struct is rendered by `ephervex account status --json` and by a
/// settings screen, and neither has any business receiving the key itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiSettings {
    /// The configured Client ID, or empty when the build's default is in use.
    pub microsoft_client_id: String,
    /// The Client ID actually being used, whichever of the two it is.
    pub effective_microsoft_client_id: String,
    /// Whether the Client ID came from the user's setting rather than the
    /// build.
    pub microsoft_client_id_is_custom: bool,
    /// The advanced User-Agent override, or empty for the launcher's own.
    pub user_agent: String,
    /// The User-Agent actually sent.
    pub effective_user_agent: String,
    /// Whether a CurseForge API key exists in the keyring.
    pub curseforge_api_key_present: bool,
    /// Where credentials would be kept, or `None` when Secret Service is
    /// unavailable and a persistent login is therefore impossible.
    pub credential_storage: Option<CredentialStorage>,
}

/// One account, as a frontend lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSummary {
    pub id: Uuid,
    pub name: String,
    /// Microsoft or offline. Carried explicitly so no caller has to infer it
    /// from whether a credential happens to exist.
    pub kind: AccountKind,
    /// Whether this is the account a launch would use.
    pub is_default: bool,
    pub added_at: String,
    pub last_authenticated_at: String,
    /// Whether a credential exists that would survive a restart.
    ///
    /// Always false for an offline account, which has no credential to store —
    /// not a missing one.
    pub has_stored_credential: bool,
}

impl AccountSummary {
    /// Whether this identity was proved to Mojang.
    #[must_use]
    pub const fn is_authenticated(&self) -> bool {
        self.kind.is_authenticated()
    }
}

/// What the launcher knows about its accounts without asking anyone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStatus {
    /// The default account, or `None` when there are none.
    pub active: Option<AccountSummary>,
    /// Every account, default first and then by name.
    pub accounts: Vec<AccountSummary>,
    /// Whether this process currently holds a usable session.
    pub session_active: bool,
    /// Where credentials are kept, when that could be determined. `None` means
    /// no Secret Service — which blocks a *Microsoft* login but not an offline
    /// account, since the latter has nothing to store.
    pub credential_storage: Option<CredentialStorage>,
}

impl AccountStatus {
    /// Whether any account exists at all.
    #[must_use]
    pub const fn is_signed_in(&self) -> bool {
        self.active.is_some()
    }
}

/// How a caller wants a login performed.
#[derive(Debug, Clone, Default)]
pub struct LoginOptions {
    pub request: LoginRequest,
    /// Proceed without secure storage, keeping the credential in memory only.
    ///
    /// Defaults to `false`, and that default is the whole point: Ephervex does not allow
    /// degrading silently, so a login on a machine with no Secret Service fails
    /// with [`AuthError::SecretStorageUnavailable`] until the caller has told
    /// the user and been told to go ahead.
    pub allow_session_only: bool,
}

impl App {
    // -- settings ----------------------------------------------------------

    /// The API settings, with both what is configured and what is in effect.
    pub fn api_settings(&self) -> CoreResult<ApiSettings> {
        let api = &self.config().api;
        let configured = api.microsoft_client_id().unwrap_or_default();
        let storage = credential_store().ok();
        Ok(ApiSettings {
            microsoft_client_id: configured.to_owned(),
            effective_microsoft_client_id: ephervex_auth::resolve_client_id(configured).to_owned(),
            microsoft_client_id_is_custom: !configured.is_empty(),
            user_agent: api.user_agent().unwrap_or_default().to_owned(),
            effective_user_agent: self.effective_user_agent(),
            curseforge_api_key_present: match &storage {
                Some(store) => ephervex_auth::curseforge_api_key(store.as_ref())?.is_some(),
                None => false,
            },
            credential_storage: storage.map(|store| store.storage()),
        })
    }

    /// Sets the Microsoft Client ID, or clears it back to the build's default.
    ///
    /// The value is validated as a GUID before it is written: Microsoft rejects
    /// anything else with an `AADSTS900023` that arrives three requests into a
    /// login and blames the tenant.
    pub fn set_microsoft_client_id(&mut self, client_id: Option<&str>) -> CoreResult<()> {
        let value = client_id.map(str::trim).unwrap_or_default();
        if !value.is_empty() && Uuid::parse_str(value).is_err() {
            return Err(CoreError::Auth(AuthError::ClientIdInvalid));
        }
        self.mutate_config(|config| value.clone_into(&mut config.api.microsoft_client_id))
    }

    /// Sets the advanced User-Agent override, or clears it.
    pub fn set_custom_user_agent(&mut self, user_agent: Option<&str>) -> CoreResult<()> {
        let value = user_agent.map(str::trim).unwrap_or_default().to_owned();
        if !value.is_empty() {
            ephervex_download::build_client(&ephervex_download::HttpConfig {
                user_agent: value.clone(),
                ..ephervex_download::HttpConfig::default()
            })
            .map_err(|source| AuthError::ClientBuild {
                source: Box::new(source),
            })?;
        }
        self.mutate_config(|config| config.api.user_agent = value)
    }

    /// Stores the CurseForge API key in the keyring, replacing any existing
    /// one.
    ///
    /// CurseForge integrations read it back through the authentication
    /// credential store. Keeping the write here lets frontends manage all API
    /// settings through `App` while ensuring the key never reaches
    /// `config.toml`. A blank value is refused, and a failed keyring write
    /// leaves whatever was stored before exactly as it was.
    ///
    /// Setting a key never contacts CurseForge: a user configuring the launcher
    /// offline must still be able to finish.
    pub fn set_curseforge_api_key(&self, key: &SecretString) -> CoreResult<()> {
        let store = credential_store()?;
        Ok(ephervex_auth::set_curseforge_api_key(store.as_ref(), key)?)
    }

    /// Removes the stored CurseForge API key.
    ///
    /// A separate operation from setting one, so that removal cannot happen by
    /// submitting an empty field — and so that it never needs the existing
    /// value.
    pub fn clear_curseforge_api_key(&self) -> CoreResult<()> {
        let store = credential_store()?;
        Ok(ephervex_auth::clear_curseforge_api_key(store.as_ref())?)
    }

    /// The `User-Agent` every outbound request carries.
    #[must_use]
    pub fn effective_user_agent(&self) -> String {
        self.config()
            .api
            .user_agent()
            .map_or_else(ephervex_model::user_agent, str::to_owned)
    }

    /// The HTTP settings this build makes requests with, honoring the
    /// advanced User-Agent override.
    #[must_use]
    pub fn http_config(&self) -> HttpConfig {
        #[allow(unused_mut)]
        let mut config = HttpConfig {
            user_agent: self.effective_user_agent(),
            ..HttpConfig::default()
        };
        // Only a test build has the field to read, so the strict transport is
        // the only one a release binary can produce.
        #[cfg(test)]
        if self.loopback_http {
            config.allow_plaintext_http = true;
        }
        config
    }

    // -- status ------------------------------------------------------------

    /// What is known about the signed-in account, without a network call.
    ///
    /// Deliberately offline: `ephervex account status` must work on a train,
    /// and a status command that hangs on a Microsoft outage is worse than no
    /// status command.
    pub fn account_status(&self) -> CoreResult<AccountStatus> {
        let store = credential_store().ok();
        let storage = store.as_ref().map(|store| store.storage());
        let default = self.config().active_account();

        let mut accounts = Vec::new();
        for (id, account) in &self.config().accounts {
            // A row whose key is not a UUID was hand-edited into the file. Skip
            // it rather than failing the whole listing: the other accounts are
            // still perfectly usable.
            let Ok(parsed) = Uuid::parse_str(id) else {
                warn!(id, "ignoring an account row whose id is not a uuid");
                continue;
            };

            // Only Microsoft accounts have anything in the keyring, so an
            // offline-only installation never touches Secret Service here.
            let has_stored_credential = match (&store, account.kind.has_stored_credential()) {
                (Some(store), true) => ephervex_auth::CredentialStore::load(
                    store.as_ref(),
                    &ephervex_auth::CredentialKey::MicrosoftRefreshToken(parsed),
                )?
                .is_some(),
                _ => false,
            };

            accounts.push(AccountSummary {
                id: parsed,
                name: account.name.clone(),
                kind: account.kind,
                is_default: default == Some(id.as_str()),
                added_at: account.added_at.clone(),
                last_authenticated_at: account.last_authenticated_at.clone(),
                has_stored_credential,
            });
        }

        // Default first, then alphabetically: the account a launch would use is
        // the one being looked for, and stable order beats UUID order.
        accounts.sort_by(|left, right| {
            right
                .is_default
                .cmp(&left.is_default)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.id.cmp(&right.id))
        });

        Ok(AccountStatus {
            active: accounts.iter().find(|account| account.is_default).cloned(),
            accounts,
            session_active: self.session_is_usable(),
            credential_storage: storage,
        })
    }

    /// One account, by UUID or display name.
    pub fn account(&self, reference: &str) -> CoreResult<AccountSummary> {
        let id = self.resolve_account(reference)?;
        self.account_summary(id)
    }

    /// Every account this installation has.
    pub fn accounts(&self) -> CoreResult<Vec<AccountSummary>> {
        Ok(self.account_status()?.accounts)
    }

    /// Resolves a UUID or display name to exactly one account.
    pub fn resolve_account(&self, reference: &str) -> CoreResult<Uuid> {
        let id = self.resolve_account_key(reference)?;
        Uuid::parse_str(id).map_err(|_| {
            CoreError::Auth(AuthError::AccountUnknown {
                reference: reference.to_owned(),
            })
        })
    }

    fn resolve_account_key(&self, reference: &str) -> CoreResult<&str> {
        self.config().find_account(reference).map_err(|lookup| {
            CoreError::Auth(match lookup {
                AccountLookup::Unknown => AuthError::AccountUnknown {
                    reference: reference.to_owned(),
                },
                AccountLookup::Ambiguous { matches } => AuthError::AccountAmbiguous {
                    reference: reference.to_owned(),
                    matches,
                },
            })
        })
    }

    /// The live session, if this process has one that is still usable.
    #[must_use]
    pub fn session(&self) -> Option<&Session> {
        self.session
            .as_ref()
            .filter(|session| session.is_usable(TOKEN_REFRESH_MARGIN))
    }

    fn session_is_usable(&self) -> bool {
        self.session().is_some()
    }

    // -- operations --------------------------------------------------------

    /// Signs in, storing the refresh credential and remembering the account.
    #[instrument(skip(self, options, observer, cancel))]
    pub async fn login(
        &mut self,
        options: &LoginOptions,
        observer: &AuthObserver,
        cancel: &CancellationToken,
    ) -> CoreResult<&Session> {
        let provider = self.build_account_provider(options.allow_session_only)?;
        let session = provider.login(&options.request, observer, cancel).await?;
        self.adopt(session)
    }

    /// Re-establishes a session for the account already signed in.
    ///
    /// This is both "app restart preserves login" and "refresh before
    /// launch"; they are the same operation, and having one implementation is
    /// what stops the two drifting.
    #[instrument(skip(self, observer, cancel))]
    pub async fn resume_account(
        &mut self,
        observer: &AuthObserver,
        cancel: &CancellationToken,
    ) -> CoreResult<&Session> {
        if self.default_account_kind()? == AccountKind::Offline {
            // Nothing to resume: an offline account has no credential and never
            // expires. Say so rather than failing later inside the keyring.
            return self.establish_offline_session();
        }
        let id = self.active_account_id()?;
        let provider = self.build_account_provider(true)?;
        let session = provider.resume(id, observer, cancel).await?;
        self.adopt(session)
    }

    /// A session that is ready to launch with, refreshing if it has to.
    ///
    /// Vanilla launch support's entry point. A caller gets a usable token or a typed reason,
    /// and [`AuthError::requires_interactive_login`] is what turns the latter
    /// into `LaunchError::AuthenticationRequired`.
    pub async fn authenticated_session(
        &mut self,
        observer: &AuthObserver,
        cancel: &CancellationToken,
    ) -> CoreResult<&Session> {
        if self.session_is_usable() {
            // Borrowck cannot see that the branch above proves this is `Some`,
            // so the check is repeated rather than the session held across it.
            return self
                .session()
                .ok_or(CoreError::Auth(AuthError::NotSignedIn));
        }

        // An offline account has nothing to refresh and nobody to ask. Routing
        // it through `resume` would demand a keyring entry that by definition
        // does not exist, and fail on a machine that never needed one.
        if self.default_account_kind()? == AccountKind::Offline {
            return self.establish_offline_session();
        }
        self.resume_account(observer, cancel).await
    }

    /// Builds the session for the default offline account.
    ///
    /// Cheap and infallible in practice: an offline identity is its name and
    /// its derived UUID, both of which are already in `config.toml`.
    fn establish_offline_session(&mut self) -> CoreResult<&Session> {
        let (id, name) = {
            let (id, account) = self
                .config()
                .active_account_config()
                .ok_or(CoreError::Auth(AuthError::NotSignedIn))?;
            (
                Uuid::parse_str(id).map_err(|_| CoreError::Auth(AuthError::NotSignedIn))?,
                account.name.clone(),
            )
        };

        self.session = Some(Session::offline(ephervex_auth::Account::offline(id, name)));
        self.session
            .as_ref()
            .ok_or(CoreError::Auth(AuthError::NotSignedIn))
    }

    /// The kind of the default account.
    fn default_account_kind(&self) -> CoreResult<AccountKind> {
        self.config()
            .active_account_config()
            .map(|(_, account)| account.kind)
            .ok_or(CoreError::Auth(AuthError::NotSignedIn))
    }

    /// Adds a local identity with no credential.
    ///
    /// The UUID is *derived* from the name the way a Minecraft server derives
    /// it in offline mode, never invented — see `ephervex_auth::offline` for
    /// why that matters for a launcher that also runs linked test servers.
    ///
    /// Needs no network and no keyring, so it works on a machine with neither.
    #[instrument(skip(self))]
    pub fn add_offline_account(&mut self, name: &str) -> CoreResult<AccountSummary> {
        let account = ephervex_auth::OfflineAccount::new(name)?;

        // Refused rather than silently returning the existing one: the UUID is
        // a hash of the name, so a "second" account of the same name would be
        // the same player, and quietly handing back the first would hide that.
        if self.config().has_account_named(&account.name) {
            return Err(AuthError::OfflineAccountExists { name: account.name }.into());
        }

        let id = account.id.to_string();
        let name = account.name.clone();
        let at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        self.mutate_config(|config| {
            config.remember_account(&id, &name, AccountKind::Offline, &at);
        })?;

        debug!(account = %account.id, "added an offline account");
        self.account_summary(account.id)
    }

    /// Makes an account the default a launch uses.
    #[instrument(skip(self))]
    pub fn set_default_account(&mut self, reference: &str) -> CoreResult<AccountSummary> {
        let key = self.resolve_account_key(reference)?.to_owned();
        let id = Uuid::parse_str(&key).map_err(|_| {
            CoreError::Auth(AuthError::AccountUnknown {
                reference: reference.to_owned(),
            })
        })?;
        self.mutate_config(|config| {
            config.set_active_account(&key);
        })?;

        // A session belonging to the account that *was* default is no longer
        // the one a launch should use, and keeping it would let a launch run as
        // the previous account without saying so.
        if self.session.as_ref().is_some_and(|s| s.account.id != id) {
            self.session = None;
        }
        self.account_summary(id)
    }

    /// Removes one account: its credential first, then its metadata.
    ///
    /// Returns the account that is default afterwards, if any.
    ///
    /// # The ordering is the guarantee
    ///
    /// The keyring entry goes first and a failure there aborts the whole
    /// operation. Doing it the other way round would mean a failed deletion
    /// left a secret in the keyring with nothing in `config.toml` recording
    /// that it exists — an orphan nobody could find to remove later. This way
    /// the worst case is an account that is still listed and can be removed
    /// again (ADR 0004).
    ///
    /// Only *this* account's entry is touched. `CredentialKey` is keyed per
    /// account precisely so removing one cannot disturb another.
    #[instrument(skip(self))]
    pub fn remove_account(&mut self, reference: &str) -> CoreResult<Option<AccountSummary>> {
        let key = self.resolve_account_key(reference)?.to_owned();
        let id = Uuid::parse_str(&key).map_err(|_| {
            CoreError::Auth(AuthError::AccountUnknown {
                reference: reference.to_owned(),
            })
        })?;
        let kind = self
            .config()
            .accounts
            .get(&key)
            .map_or(AccountKind::Microsoft, |account| account.kind);

        if kind.has_stored_credential() {
            match credential_store() {
                Ok(store) => ephervex_auth::CredentialStore::delete(
                    store.as_ref(),
                    &ephervex_auth::CredentialKey::MicrosoftRefreshToken(id),
                )?,
                // No Secret Service at all means there is nothing persistent to
                // orphan. Refusing here would strand an account that a user on
                // such a machine could never remove.
                Err(error) => debug!(%error, "no keyring to remove a credential from"),
            }
        }

        self.mutate_config(|config| config.forget_account(&key))?;
        if self.session.as_ref().is_some_and(|s| s.account.id == id) {
            self.session = None;
        }

        match self.config().active_account() {
            Some(remaining) => {
                let remaining = Uuid::parse_str(remaining).ok();
                remaining.map(|id| self.account_summary(id)).transpose()
            }
            None => Ok(None),
        }
    }

    /// Signs out of the default account: removes its credential and forgets it.
    ///
    /// Exactly [`Self::remove_account`] aimed at whichever account is default,
    /// which is what "log out" means once there can be more than one. Returns
    /// whether there was anything to sign out of.
    #[instrument(skip(self))]
    pub fn logout(&mut self) -> CoreResult<bool> {
        let Some(id) = self.config().active_account().map(str::to_owned) else {
            self.session = None;
            return Ok(false);
        };
        self.remove_account(&id)?;
        Ok(true)
    }

    /// One account, by id.
    fn account_summary(&self, id: Uuid) -> CoreResult<AccountSummary> {
        self.account_status()?
            .accounts
            .into_iter()
            .find(|account| account.id == id)
            .ok_or_else(|| {
                CoreError::Auth(AuthError::AccountUnknown {
                    reference: id.to_string(),
                })
            })
    }

    // -- construction ------------------------------------------------------

    /// Builds a provider, choosing where credentials go.
    ///
    /// `allow_session_only` is the explicit consent gate. With it `false` — the
    /// default — a machine with no Secret Service gets a clear refusal that
    /// says session-only login is the alternative, rather than a login that
    /// silently evaporates on exit.
    fn build_account_provider(
        &self,
        allow_session_only: bool,
    ) -> CoreResult<MicrosoftAccountProvider> {
        let store: Arc<dyn CredentialStore> = match KeyringStore::open() {
            Ok(keyring) => Arc::new(keyring),
            Err(error) if allow_session_only => {
                warn!(%error, "signing in for this session only; no secure storage is available");
                Arc::new(MemoryStore::new())
            }
            Err(error) => return Err(error.into()),
        };

        Ok(MicrosoftAccountProvider::with_endpoints(
            ephervex_auth::resolve_client_id(
                self.config().api.microsoft_client_id().unwrap_or_default(),
            ),
            store,
            ephervex_auth::Endpoints::production(),
            &self.http_config(),
        )?)
    }

    /// The active account's id, or a typed "nobody is signed in".
    fn active_account_id(&self) -> CoreResult<Uuid> {
        let id = self
            .config()
            .active_account()
            .ok_or(CoreError::Auth(AuthError::NotSignedIn))?;
        Uuid::parse_str(id).map_err(|_| CoreError::Auth(AuthError::NotSignedIn))
    }

    /// Records a completed session: metadata to the file, token to memory.
    fn adopt(&mut self, session: Session) -> CoreResult<&Session> {
        let id = session.account.id.to_string();
        let name = session.account.name.clone();
        let at = session
            .account
            .authenticated_at
            .to_rfc3339_opts(SecondsFormat::Secs, true);

        // Only for a login that will still be here next time. A session-only
        // login writing an account into `config.toml` would make the next run
        // claim a login whose credential does not exist.
        if session
            .storage()
            .is_some_and(CredentialStorage::is_persistent)
        {
            self.mutate_config(|config| {
                config.remember_account(&id, &name, session.account.kind, &at);
            })?;
        } else {
            debug!(account = %id, "session-only login; nothing written to the configuration");
        }

        self.session = Some(session);
        self.session
            .as_ref()
            .ok_or(CoreError::Auth(AuthError::NotSignedIn))
    }
}

/// The credential store, when Secret Service answers.
///
/// A free function rather than a method because it depends on the machine
/// rather than on the application: `keyring` does its own one-time platform
/// initialization behind a `LazyLock`, so calling this repeatedly costs a
/// struct literal and nothing more.
pub(crate) fn credential_store() -> Result<Arc<dyn CredentialStore>, AuthError> {
    KeyringStore::open().map(|store| -> Arc<dyn CredentialStore> { Arc::new(store) })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_storage::Paths;

    use super::*;

    fn app_rooted_at(base: &std::path::Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app opens")
    }

    #[test]
    fn the_api_settings_report_the_builds_default_client_id_until_one_is_set() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());

        let settings = app.api_settings().expect("settings");
        assert!(settings.microsoft_client_id.is_empty());
        assert!(!settings.microsoft_client_id_is_custom);
        assert_eq!(
            settings.effective_microsoft_client_id,
            ephervex_auth::DEFAULT_MICROSOFT_CLIENT_ID
        );
        assert!(settings.effective_user_agent.starts_with("Ephervex/"));

        app.set_microsoft_client_id(Some("11112222-bbbb-3333-cccc-4444dddd5555"))
            .expect("set");
        let settings = app.api_settings().expect("settings");
        assert!(settings.microsoft_client_id_is_custom);
        assert_eq!(
            settings.effective_microsoft_client_id,
            "11112222-bbbb-3333-cccc-4444dddd5555"
        );

        // Clearing the field returns to the build's default rather than
        // authenticating as nobody.
        app.set_microsoft_client_id(None).expect("clear");
        assert_eq!(
            app.api_settings()
                .expect("settings")
                .effective_microsoft_client_id,
            ephervex_auth::DEFAULT_MICROSOFT_CLIENT_ID
        );
    }

    #[test]
    fn a_client_id_that_is_not_a_guid_is_refused_at_the_settings_boundary() {
        // Microsoft's own refusal arrives three requests into a login and
        // blames the tenant, which sends the user looking in the wrong place.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        assert!(app.set_microsoft_client_id(Some("not-a-guid")).is_err());
        assert!(
            app.api_settings()
                .expect("settings")
                .microsoft_client_id
                .is_empty(),
            "a refused value must not be written"
        );
    }

    #[test]
    fn the_custom_user_agent_reaches_the_http_configuration() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        assert!(app.http_config().user_agent.starts_with("Ephervex/"));

        app.set_custom_user_agent(Some("BugReportBot/2.0 (+https://example.invalid)"))
            .expect("set");
        assert_eq!(
            app.http_config().user_agent,
            "BugReportBot/2.0 (+https://example.invalid)"
        );

        app.set_custom_user_agent(None).expect("clear");
        let reset = app.api_settings().expect("settings");
        assert!(reset.user_agent.is_empty());
        assert!(reset.effective_user_agent.starts_with("Ephervex/"));
        assert_eq!(app.http_config().user_agent, reset.effective_user_agent);
    }

    #[test]
    fn invalid_api_identifiers_are_refused_without_echoing_or_persisting_them() {
        const CREDENTIAL: &str = "UNIQUE-CREDENTIAL-MATERIAL";
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());

        let error = app
            .set_microsoft_client_id(Some(CREDENTIAL))
            .expect_err("not a GUID");
        assert!(!format!("{error:?} {error}").contains(CREDENTIAL));

        app.set_custom_user_agent(Some("ValidAgent/1.0"))
            .expect("valid agent");
        let error = app
            .set_custom_user_agent(Some("bad\r\nagent"))
            .expect_err("header controls are invalid");
        assert_eq!(
            app.api_settings().expect("settings").user_agent,
            "ValidAgent/1.0",
            "a rejected value must not replace the usable configuration"
        );
        assert!(matches!(
            error,
            CoreError::Auth(AuthError::ClientBuild { .. })
        ));
    }

    #[test]
    fn api_settings_survive_a_restart_and_never_reach_the_file_as_a_secret() {
        let base = tempfile::tempdir().expect("temp dir");
        {
            let mut app = app_rooted_at(base.path());
            app.set_microsoft_client_id(Some("11112222-bbbb-3333-cccc-4444dddd5555"))
                .expect("set");
        }
        let app = app_rooted_at(base.path());
        assert_eq!(
            app.api_settings().expect("settings").microsoft_client_id,
            "11112222-bbbb-3333-cccc-4444dddd5555"
        );

        let written = std::fs::read_to_string(app.paths().config_file()).expect("read");
        let lowered = written.to_ascii_lowercase();
        for marker in ephervex_storage::SECRET_KEY_MARKERS {
            assert!(!lowered.contains(marker), "`{marker}`:\n{written}");
        }
    }

    #[test]
    fn a_fresh_installation_reports_no_accounts() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let status = app.account_status().expect("status");
        assert!(!status.is_signed_in());
        assert!(status.active.is_none());
        assert!(status.accounts.is_empty());
        assert!(!status.session_active);
    }

    #[test]
    fn an_offline_account_needs_no_network_and_no_keyring() {
        // The whole flow: a name, and that is it. This must work on a machine
        // with no Secret Service and no connection, which is exactly the
        // machine Vanilla launch support will be manually verified on while approval is
        // pending.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());

        let account = app.add_offline_account("Notch").expect("added");
        assert_eq!(account.name, "Notch");
        assert_eq!(account.kind, AccountKind::Offline);
        assert!(!account.is_authenticated());
        assert!(
            !account.has_stored_credential,
            "there is no credential, which is not the same as one that failed to save"
        );
        // Derived, never invented — a server computes the same value.
        assert_eq!(
            account.id.to_string(),
            "b50ad385-829d-3141-a216-7e7d7539ba7f"
        );

        // The first account becomes the default on its own.
        assert!(account.is_default);
    }

    #[test]
    fn an_offline_account_survives_a_restart_and_stores_no_secret() {
        let base = tempfile::tempdir().expect("temp dir");
        {
            let mut app = app_rooted_at(base.path());
            app.add_offline_account("Notch").expect("added");
        }
        let app = app_rooted_at(base.path());
        let status = app.account_status().expect("status");
        let active = status.active.expect("still there after a restart");
        assert_eq!(active.name, "Notch");
        assert_eq!(active.kind, AccountKind::Offline);

        let written = std::fs::read_to_string(app.paths().config_file()).expect("read");
        let lowered = written.to_ascii_lowercase();
        for marker in ephervex_storage::SECRET_KEY_MARKERS {
            assert!(!lowered.contains(marker), "`{marker}`:\n{written}");
        }
    }

    #[test]
    fn a_duplicate_offline_name_is_refused_rather_than_deduplicated() {
        // The UUID is a hash of the name, so a "second" account of the same
        // name is the same player. Quietly returning the first would hide that
        // from someone who thought they were adding another identity.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("Notch").expect("first");

        let error = app.add_offline_account("notch").expect_err("must refuse");
        assert_eq!(error.message_id(), "error-auth-offline-account-exists");
        assert_eq!(app.accounts().expect("list").len(), 1);
    }

    #[test]
    fn an_invalid_offline_name_is_refused_at_creation() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let error = app.add_offline_account("no").expect_err("too short");
        assert_eq!(error.message_id(), "error-auth-offline-name-invalid");
        assert!(app.accounts().expect("list").is_empty());
    }

    #[test]
    fn several_accounts_coexist_and_exactly_one_is_the_default() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("Alice").expect("added");
        app.add_offline_account("Bob").expect("added");
        app.add_offline_account("Carol").expect("added");

        let status = app.account_status().expect("status");
        assert_eq!(status.accounts.len(), 3);
        assert_eq!(
            status.accounts.iter().filter(|a| a.is_default).count(),
            1,
            "exactly one default, always"
        );
        // The first one added keeps it; adding more must not steal it.
        assert_eq!(status.active.expect("a default").name, "Alice");
        // The default is listed first, because it is what a user is looking for.
        assert!(status.accounts[0].is_default);
    }

    #[test]
    fn the_default_can_be_chosen_by_name_or_by_uuid() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("Alice").expect("added");
        let bob = app.add_offline_account("Bob").expect("added");

        let chosen = app.set_default_account("Bob").expect("by name");
        assert_eq!(chosen.id, bob.id);
        assert!(chosen.is_default);

        app.set_default_account("Alice").expect("back again");
        let chosen = app
            .set_default_account(&bob.id.to_string())
            .expect("by uuid");
        assert_eq!(chosen.name, "Bob");
    }

    #[test]
    fn choosing_an_account_that_does_not_exist_says_so() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("Alice").expect("added");

        let error = app.set_default_account("Nobody").expect_err("unknown");
        assert_eq!(error.message_id(), "error-auth-account-unknown");
    }

    #[test]
    fn removing_the_default_promotes_a_survivor_rather_than_leaving_none() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("Alice").expect("added");
        app.add_offline_account("Bob").expect("added");

        let remaining = app.remove_account("Alice").expect("removed");
        let remaining = remaining.expect("one account is left, so one is default");
        assert_eq!(remaining.name, "Bob");
        assert!(remaining.is_default);
        assert_eq!(app.accounts().expect("list").len(), 1);
    }

    #[test]
    fn removing_a_non_default_account_leaves_the_default_alone() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("Alice").expect("added");
        app.add_offline_account("Bob").expect("added");

        let remaining = app
            .remove_account("Bob")
            .expect("removed")
            .expect("default");
        assert_eq!(remaining.name, "Alice");
    }

    #[test]
    fn removing_the_last_account_leaves_none() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("Alice").expect("added");

        assert!(app.remove_account("Alice").expect("removed").is_none());
        assert!(app.accounts().expect("list").is_empty());
        assert!(!app.account_status().expect("status").is_signed_in());
    }

    #[test]
    fn removing_one_offline_account_does_not_disturb_another() {
        // `CredentialKey` is keyed per account precisely so that removing one
        // cannot reach another's entry.
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let alice = app.add_offline_account("Alice").expect("added");
        app.add_offline_account("Bob").expect("added");

        app.remove_account("Bob").expect("removed");
        let accounts = app.accounts().expect("list");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, alice.id);
    }

    #[tokio::test]
    async fn an_offline_default_produces_a_session_without_touching_the_network() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let added = app.add_offline_account("Notch").expect("added");

        let session = app
            .authenticated_session(&AuthObserver::silent(), &CancellationToken::new())
            .await
            .expect("an offline account is always ready to launch");

        assert_eq!(session.account.id, added.id);
        assert_eq!(session.account.name, "Notch");
        // The property that must never regress: an offline identity proves
        // nothing and must not be presentable to an online-mode server.
        assert!(!session.is_authenticated());
        assert!(session.access_token().is_none());
        assert!(session.expires_at().is_none());
    }

    #[test]
    fn an_ambiguous_name_makes_the_user_disambiguate() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("Alice").expect("added");

        // Two accounts cannot share a name through `add_offline_account`, so
        // the ambiguous case is constructed the way a Microsoft login would
        // create it: a real account whose profile name matches an offline one.
        app.mutate_config(|config| {
            config.remember_account(
                "069a79f4-44e9-4726-a5be-fca90e38aaf5",
                "Alice",
                AccountKind::Microsoft,
                "2026-08-08T12:00:00Z",
            );
        })
        .expect("seed");

        let error = app.set_default_account("Alice").expect_err("ambiguous");
        assert_eq!(error.message_id(), "error-auth-account-ambiguous");
        // The UUID still resolves, which is the escape hatch the message names.
        assert!(
            app.set_default_account("069a79f4-44e9-4726-a5be-fca90e38aaf5")
                .is_ok()
        );
    }

    #[test]
    fn canonical_row_id_resolves_an_account_stored_with_a_compact_uuid() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("DefaultPlayer").expect("added");
        app.mutate_config(|config| {
            config.remember_account(
                "069a79f444e94726a5befca90e38aaf5",
                "MelanX",
                AccountKind::Microsoft,
                "2026-08-08T12:00:00Z",
            );
        })
        .expect("seed Microsoft account");

        let row = app
            .accounts()
            .expect("visible accounts")
            .into_iter()
            .find(|account| account.name == "MelanX")
            .expect("Microsoft account row");
        let row_id = row.id.to_string();

        let selected = app
            .set_default_account(&row_id)
            .expect("make default by row id");
        assert_eq!(selected.id, row.id);
        assert!(selected.is_default);
        app.remove_account(&row_id).expect("remove by row id");
        assert!(
            app.accounts()
                .expect("remaining accounts")
                .iter()
                .all(|account| account.id != row.id)
        );
    }

    #[test]
    fn signing_out_when_nobody_is_signed_in_succeeds_and_reports_nothing_to_do() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        assert!(!app.logout().expect("logout"), "nothing to sign out of");
    }

    #[tokio::test]
    async fn resuming_with_no_account_asks_for_a_login_rather_than_reaching_the_network() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());

        let error = app
            .resume_account(&AuthObserver::silent(), &CancellationToken::new())
            .await
            .expect_err("nobody is signed in");
        assert_eq!(error.message_id(), "error-auth-not-signed-in");
        assert!(error.requires_interactive_login());
    }

    #[test]
    fn the_default_login_options_refuse_to_degrade_without_being_asked() {
        // session-only login is offered, never assumed.
        assert!(!LoginOptions::default().allow_session_only);
    }
}
