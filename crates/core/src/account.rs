//! The account services both frontends call.
//!
//! # What this layer adds over `ephervex-auth`
//!
//! The auth crate knows how to run the token chain. It deliberately knows
//! nothing about configuration files, keyrings-versus-settings, or which
//! account is "the" account - those are application questions, and answering
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
//! Logging in changes persistent state - the active account, its name, when it
//! last authenticated - so these take `&mut self` while the read-only services
//! elsewhere in [`crate::App`] take `&self`. That is the honest signature for
//! an operation that writes the configuration file, and it costs nothing today:
//! the CLI owns its `App` outright, and the GUI owns one inside
//! its worker rather than sharing it across repaints.

use std::sync::Arc;

use chrono::{SecondsFormat, TimeDelta, Utc};
use ephervex_auth::{
    AccountProvider, AuthError, AuthObserver, AuthResult, CredentialChangeGuard,
    CredentialChangeOrder, CredentialKey, CredentialRemoval, CredentialStorage, CredentialStore,
    KeyringStore, Login, LoginRequest, MemoryStore, MicrosoftAccountProvider, ProcessChangeOrder,
    SecretString, Session,
};
use ephervex_download::HttpConfig;
use ephervex_model::AccountKind;
use ephervex_storage::{AccountLookup, ConfigError, CredentialChangeLock, Paths};
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument, warn};
use uuid::Uuid;

use crate::App;
use crate::error::{CoreError, CoreResult};

/// Orders credential changes across every process that shares one
/// configuration directory (ADR 0004).
///
/// The keyring is per user and the account records are per configuration
/// directory, so the order is keyed by the directory that holds the records.
#[derive(Debug)]
struct ConfigDirectoryOrder {
    paths: Paths,
}

impl CredentialChangeOrder for ConfigDirectoryOrder {
    fn hold(&self) -> AuthResult<CredentialChangeGuard> {
        CredentialChangeLock::take(&self.paths)
            .map(CredentialChangeGuard::new)
            .map_err(|error| match error {
                ConfigError::Busy { path } => AuthError::CredentialChangeBusy { path },
                ConfigError::Unwritable { path, .. } => {
                    AuthError::CredentialChangeUnavailable { path }
                }
                _ => AuthError::CredentialChangeUnavailable {
                    path: self.paths.config_dir().to_path_buf(),
                },
            })
    }
}

/// How much life a Minecraft token needs left on it to be worth reusing.
///
/// A launch takes tens of seconds to reach the point of needing the token, and
/// a token that expires part-way through produces a disconnect that looks like
/// a server problem. Refreshing early is cheap; refreshing late is not.
pub const TOKEN_REFRESH_MARGIN: TimeDelta = TimeDelta::minutes(5);

/// Which CurseForge API key an operation would use.
///
/// One value rather than a pair of booleans, because only three of the four
/// combinations they could express exist: a key cannot be configured and
/// absent at the same time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CurseForgeKeyState {
    /// The user stored one, and it is the one used.
    Configured,
    /// None stored, so the key this build ships is the one used (ADR 0030).
    BuildDefault,
    /// None stored and none shipped: CurseForge requests have no credential.
    ///
    /// The default, because a value nobody has filled in yet is one that knows
    /// of no key.
    #[default]
    Missing,
}

impl CurseForgeKeyState {
    /// Whether any key is in effect.
    #[must_use]
    pub const fn is_present(self) -> bool {
        matches!(self, Self::Configured | Self::BuildDefault)
    }

    /// Whether the user configured the key in effect, which is the only case
    /// where there is something for a `Remove` action to remove.
    #[must_use]
    pub const fn is_configured(self) -> bool {
        matches!(self, Self::Configured)
    }

    /// A stable identifier for `--json` output and frontend state.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::BuildDefault => "build-default",
            Self::Missing => "missing",
        }
    }
}

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
    /// Which CurseForge key is in effect, if any.
    pub curseforge_key: CurseForgeKeyState,
    /// Whether this build carries a CurseForge key of its own (ADR 0030).
    ///
    /// Separate from [`curseforge_key`](Self::curseforge_key), which says what
    /// is *in use*: this stays true while a user's own key is the one in use,
    /// because it is a fact about the build. What it answers is whether
    /// removing a configured key leaves CurseForge working or not.
    pub curseforge_default_key_available: bool,
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
    pub credential: CredentialPresence,
}

/// Whether an account has a credential saved in the keyring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialPresence {
    /// A credential is saved and would survive a restart.
    Stored,
    /// The keyring answered and holds no credential for the account.
    Missing,
    /// The keyring could not be asked, so a credential may well be saved.
    Unknown,
    /// The account has no credential by construction, such as an offline one.
    NotApplicable,
}

impl CredentialPresence {
    /// A stable machine name for scripted output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stored => "stored",
            Self::Missing => "missing",
            Self::Unknown => "unknown",
            Self::NotApplicable => "not-applicable",
        }
    }
}

impl AccountSummary {
    /// Whether this identity was proved to Mojang.
    #[must_use]
    pub const fn is_authenticated(&self) -> bool {
        self.kind.is_authenticated()
    }
}

/// What became of an account's stored credential when the account was removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialOutcome {
    /// A saved credential was found in the keyring and deleted.
    Deleted,
    /// The keyring could not be reached, so any credential it holds is
    /// recorded for deletion once it can be. It may still be there.
    ScheduledForDeletion,
    /// There was no saved credential: an offline account, a session-only
    /// sign-in, or a keyring with no entry for the account.
    NoneStored,
}

impl CredentialOutcome {
    /// A stable machine name for scripted output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deleted => "deleted",
            Self::ScheduledForDeletion => "scheduled-for-deletion",
            Self::NoneStored => "none-stored",
        }
    }
}

/// The result of removing one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRemoval {
    /// The account that is default afterwards, if any.
    pub remaining: Option<AccountSummary>,
    /// What happened to the removed account's stored credential.
    pub credential: CredentialOutcome,
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
    /// no Secret Service - which blocks a *Microsoft* login but not an offline
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
        let storage = self.keyring().ok();
        Ok(ApiSettings {
            microsoft_client_id: configured.to_owned(),
            effective_microsoft_client_id: ephervex_auth::resolve_client_id(configured).to_owned(),
            microsoft_client_id_is_custom: !configured.is_empty(),
            user_agent: api.user_agent().unwrap_or_default().to_owned(),
            effective_user_agent: self.effective_user_agent(),
            curseforge_key: match &storage {
                Some(store) => {
                    if ephervex_auth::curseforge_api_key_configured(store.as_ref())? {
                        CurseForgeKeyState::Configured
                    } else if ephervex_auth::curseforge_api_key(store.as_ref())?.is_some() {
                        CurseForgeKeyState::BuildDefault
                    } else {
                        CurseForgeKeyState::Missing
                    }
                }
                None if ephervex_auth::curseforge_api_key_without_store().is_some() => {
                    CurseForgeKeyState::BuildDefault
                }
                None => CurseForgeKeyState::Missing,
            },
            curseforge_default_key_available: !ephervex_auth::DEFAULT_CURSEFORGE_API_KEY.is_empty(),
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
        let store = self.keyring()?;
        Ok(ephervex_auth::set_curseforge_api_key(store.as_ref(), key)?)
    }

    /// Removes the stored CurseForge API key.
    ///
    /// A separate operation from setting one, so that removal cannot happen by
    /// submitting an empty field - and so that it never needs the existing
    /// value.
    pub fn clear_curseforge_api_key(&self) -> CoreResult<()> {
        let store = self.keyring()?;
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
        let store = self.keyring().ok();
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
            let credential = if !account.kind.has_stored_credential() {
                CredentialPresence::NotApplicable
            } else if let Some(store) = &store {
                match ephervex_auth::CredentialStore::load(
                    store.as_ref(),
                    &ephervex_auth::CredentialKey::MicrosoftRefreshToken(parsed),
                ) {
                    Ok(Some(_)) => CredentialPresence::Stored,
                    Ok(None) => CredentialPresence::Missing,
                    Err(error) => {
                        warn!(%error, id, "could not read the saved credential of an account");
                        CredentialPresence::Unknown
                    }
                }
            } else {
                // An unreachable keyring says nothing about what it holds.
                CredentialPresence::Unknown
            };

            accounts.push(AccountSummary {
                id: parsed,
                name: account.name.clone(),
                kind: account.kind,
                is_default: default == Some(id.as_str()),
                added_at: account.added_at.clone(),
                last_authenticated_at: account.last_authenticated_at.clone(),
                credential,
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

    /// Whether the cached session is usable and is the one a launch with no
    /// account named runs as.
    ///
    /// The cache holds whichever account signed in or launched last, so it is
    /// the default's only when it names the default account. A session-only
    /// login is recorded nowhere, so with no default configured it is the only
    /// identity this process can launch as.
    fn session_is_default_launch(&self) -> bool {
        let Some(session) = self.session() else {
            return false;
        };
        match self.config().active_account_config() {
            Some((default, _)) => session.account.id.to_string() == default,
            None => session
                .storage()
                .is_some_and(|storage| !storage.is_persistent()),
        }
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
        // Before the login stores a new credential, which a pending cleanup
        // for the same account must not then delete.
        self.finish_keyring_cleanup();
        let provider = self.build_account_provider(options.allow_session_only)?;
        let login = provider.login(&options.request, observer, cancel).await?;
        self.adopt_login(&provider, login)
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
        self.finish_keyring_cleanup();
        let id = self.active_account_id()?;
        // A configured account lives in the keyring, so an unreachable one must
        // fail as such instead of resuming against an empty session store.
        let provider = self.build_account_provider(false)?;
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
        if self.session_is_default_launch() {
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

    /// A session ready to launch with, as one named account.
    ///
    /// The same operation as [`Self::authenticated_session`], with the account
    /// chosen rather than read from configuration. It deliberately does not
    /// change which account is the default: launching as somebody else is one
    /// launch, not a new preference, and a launcher that changed the default as
    /// a side effect would send the *next* launch somewhere unexpected.
    pub async fn session_for_account(
        &mut self,
        reference: &str,
        observer: &AuthObserver,
        cancel: &CancellationToken,
    ) -> CoreResult<&Session> {
        let id = self.resolve_account(reference)?;
        // A cached session belongs to whichever account established it, so it
        // is only reusable when that is this one.
        if self
            .session()
            .is_some_and(|session| session.account.id == id)
        {
            return self
                .session()
                .ok_or(CoreError::Auth(AuthError::NotSignedIn));
        }

        if self.account_kind(id)? == AccountKind::Offline {
            return self.establish_offline_session_for(id);
        }
        let provider = self.build_account_provider(false)?;
        let session = provider.resume(id, observer, cancel).await?;
        self.adopt(session)
    }

    /// The recorded kind of one account.
    fn account_kind(&self, id: Uuid) -> CoreResult<AccountKind> {
        self.config()
            .accounts
            .get(&id.to_string())
            .map(|account| account.kind)
            .ok_or_else(|| {
                CoreError::Auth(AuthError::AccountUnknown {
                    reference: id.to_string(),
                })
            })
    }

    /// Builds the session for the default offline account.
    ///
    /// Cheap and infallible in practice: an offline identity is its name and
    /// its derived UUID, both of which are already in `config.toml`.
    fn establish_offline_session(&mut self) -> CoreResult<&Session> {
        let id = {
            let (id, _) = self
                .config()
                .active_account_config()
                .ok_or(CoreError::Auth(AuthError::NotSignedIn))?;
            Uuid::parse_str(id).map_err(|_| CoreError::Auth(AuthError::NotSignedIn))?
        };
        self.establish_offline_session_for(id)
    }

    /// Builds the session for one offline account, whether or not it is the
    /// default.
    fn establish_offline_session_for(&mut self, id: Uuid) -> CoreResult<&Session> {
        let name = self
            .config()
            .accounts
            .get(&id.to_string())
            .map(|account| account.name.clone())
            .ok_or(CoreError::Auth(AuthError::NotSignedIn))?;

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
        self.reload_config()?;

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
        self.reload_config()?;
        let key = self.resolve_account_key(reference)?.to_owned();
        let id = Uuid::parse_str(&key).map_err(|_| {
            CoreError::Auth(AuthError::AccountUnknown {
                reference: reference.to_owned(),
            })
        })?;
        let mut known = false;
        self.mutate_config(|config| known = config.set_active_account(&key))?;
        if !known {
            return Err(AuthError::AccountUnknown {
                reference: reference.to_owned(),
            }
            .into());
        }

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
    /// Returns the account that is default afterwards, if any, and whether the
    /// credential was deleted, is only scheduled for deletion, or never existed.
    ///
    /// # The ordering is the guarantee
    ///
    /// The keyring entry goes first and a failure there aborts the whole
    /// operation. Doing it the other way round would mean a failed deletion
    /// left a secret in the keyring with nothing in `config.toml` recording
    /// that it exists — an orphan nobody could find to remove later. This way
    /// the worst case is an account that is still listed and can be removed
    /// again (ADR 0004). When the keyring cannot be reached at all, the account
    /// is forgotten and its credential recorded for deletion once it can.
    ///
    /// Only *this* account's entry is touched. `CredentialKey` is keyed per
    /// account precisely so removing one cannot disturb another.
    #[instrument(skip(self))]
    pub fn remove_account(&mut self, reference: &str) -> CoreResult<AccountRemoval> {
        // Deleting the credential and forgetting the account are one change: a
        // sign-in or resume in between would store a credential back for an
        // account that is then forgotten.
        let held = self.hold_credential_changes()?;
        self.reload_config()?;
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

        let mut keyring = None;
        let mut keyring_reachable = true;
        let mut credential = CredentialOutcome::NoneStored;
        if kind.has_stored_credential() {
            match self.keyring() {
                Ok(store) => {
                    credential = match store.delete(&CredentialKey::MicrosoftRefreshToken(id))? {
                        CredentialRemoval::Removed => CredentialOutcome::Deleted,
                        CredentialRemoval::Absent => CredentialOutcome::NoneStored,
                    };
                    keyring = Some(store);
                }
                // The keyring may only be locked or briefly gone, so the
                // credential is recorded for deletion rather than forgotten.
                // Refusing instead would strand the account on a machine whose
                // keyring never comes back.
                Err(error) => {
                    debug!(%error, "keyring unavailable; its credential is removed later");
                    keyring_reachable = false;
                    credential = CredentialOutcome::ScheduledForDeletion;
                }
            }
        }

        if keyring_reachable {
            self.mutate_config(|config| config.forget_account(&key))?;
        } else {
            // Recorded in the spelling a later sign-in remembers it by.
            let account = id.to_string();
            self.mutate_config(|config| {
                config.forget_account(&key);
                config.record_keyring_cleanup(&account);
            })?;
        }
        // Each pending deletion takes the order itself.
        drop(held);
        if keyring_reachable {
            match keyring {
                Some(store) => self.clean_up_keyring(store.as_ref()),
                None => self.finish_keyring_cleanup(),
            }
        }
        if self.session.as_ref().is_some_and(|s| s.account.id == id) {
            self.session = None;
        }

        let remaining = match self.config().active_account() {
            Some(remaining) => {
                let remaining = Uuid::parse_str(remaining).ok();
                remaining.map(|id| self.account_summary(id)).transpose()?
            }
            None => None,
        };
        Ok(AccountRemoval {
            remaining,
            credential,
        })
    }

    /// Signs out of the default account: removes its credential and forgets it.
    ///
    /// Exactly [`Self::remove_account`] aimed at whichever account is default,
    /// which is what "log out" means once there can be more than one. Returns
    /// what happened to the credential, or `None` when there was nobody to
    /// sign out.
    #[instrument(skip(self))]
    pub fn logout(&mut self) -> CoreResult<Option<CredentialOutcome>> {
        self.reload_config()?;
        let Some(id) = self.config().active_account().map(str::to_owned) else {
            self.session = None;
            return Ok(None);
        };
        Ok(Some(self.remove_account(&id)?.credential))
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
        let store: Arc<dyn CredentialStore> = match self.keyring() {
            Ok(keyring) => keyring,
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
        )?
        .with_change_order(Arc::new(self.credential_order())))
    }

    fn credential_order(&self) -> ConfigDirectoryOrder {
        ConfigDirectoryOrder {
            paths: self.paths().clone(),
        }
    }

    /// Claims the order that keeps stored credentials and the account records
    /// that name them consistent, across processes.
    ///
    /// Not reentrant, and never taken while holding the configuration lock.
    fn hold_credential_changes(&self) -> CoreResult<CredentialChangeGuard> {
        Ok(self.credential_order().hold()?)
    }

    /// The persistent keyring, or why it cannot be reached.
    #[cfg(test)]
    fn keyring(&self) -> Result<Arc<dyn CredentialStore>, AuthError> {
        self.test_keyring.open()
    }

    /// The persistent keyring, or why it cannot be reached.
    #[cfg(not(test))]
    #[expect(
        clippy::unused_self,
        reason = "a test build reads its keyring from the App"
    )]
    fn keyring(&self) -> Result<Arc<dyn CredentialStore>, AuthError> {
        credential_store()
    }

    /// Deletes the stored credentials of accounts removed while the keyring
    /// could not be reached, once it can be.
    fn finish_keyring_cleanup(&mut self) {
        if self.reload_config().is_err() || self.config().keyring_cleanup.is_empty() {
            return;
        }
        if let Ok(store) = self.keyring() {
            self.clean_up_keyring(store.as_ref());
        }
    }

    /// [`Self::finish_keyring_cleanup`] with a keyring already open.
    ///
    /// Each deletion is decided and made under the credential order against
    /// the current configuration, so a sign-in in another process that has
    /// since recorded the account keeps its new credential. The deletions
    /// happen outside the configuration lock, because one can wait on a
    /// keyring unlock prompt. A deletion that fails stays recorded for the next
    /// attempt.
    fn clean_up_keyring(&mut self, store: &dyn CredentialStore) {
        for pending in self.config().keyring_cleanup.clone() {
            if let Err(error) = self.finish_pending_cleanup(store, &pending) {
                warn!(%error, account = %pending, "could not remove a removed account's credential yet");
            }
        }
    }

    fn finish_pending_cleanup(
        &mut self,
        store: &dyn CredentialStore,
        pending: &str,
    ) -> CoreResult<()> {
        let _held = self.hold_credential_changes()?;
        self.reload_config()?;
        if !self.config().keyring_cleanup.iter().any(|p| p == pending) {
            return Ok(());
        }
        // A recorded account's credential is current, not one to clean up.
        if !self.config().accounts.contains_key(pending)
            && let Ok(account) = Uuid::parse_str(pending)
        {
            store.delete(&CredentialKey::MicrosoftRefreshToken(account))?;
        }
        self.mutate_config(|config| config.keyring_cleanup.retain(|p| p != pending))
    }

    /// The active account's id, or a typed "nobody is signed in".
    fn active_account_id(&self) -> CoreResult<Uuid> {
        let id = self
            .config()
            .active_account()
            .ok_or(CoreError::Auth(AuthError::NotSignedIn))?;
        Uuid::parse_str(id).map_err(|_| CoreError::Auth(AuthError::NotSignedIn))
    }

    /// Records a resumed session: metadata to the file, token to memory.
    ///
    /// Only an account the configuration still names is updated. One removed
    /// while the resume ran stays removed, and the resume fails as signed out.
    fn adopt(&mut self, session: Session) -> CoreResult<&Session> {
        self.refresh_recorded_account(&session)?;
        self.hold_session(session)
    }

    /// [`Self::adopt`] for a new login: stores its credential and records its
    /// account as one change under the credential order (ADR 0004).
    ///
    /// An account that is not recorded yet is first written to the pending
    /// cleanup list, which recording it clears in the same update. A login
    /// that dies between storing and recording therefore leaves a credential
    /// that a later cleanup deletes, never one nothing names. When the account
    /// cannot be recorded, the credential this login stored is deleted again.
    fn adopt_login(
        &mut self,
        provider: &MicrosoftAccountProvider,
        login: Login,
    ) -> CoreResult<&Session> {
        let persistent = login
            .session
            .storage()
            .is_some_and(CredentialStorage::is_persistent);
        // A session-only credential lives in this process, so only this
        // process can race it.
        let held = if persistent {
            self.hold_credential_changes()?
        } else {
            ProcessChangeOrder.hold()?
        };
        let id = login.session.account.id;
        let account = id.to_string();

        let mut stores_new_account = false;
        if persistent && login.has_refresh_credential() {
            self.reload_config()?;
            stores_new_account = !self.config().accounts.contains_key(&account);
            if stores_new_account {
                self.mutate_config(|config| config.record_keyring_cleanup(&account))?;
            }
        }
        // A failed store may still have written the secret, and the entry may
        // already name an earlier credential, so it stays for a cleanup.
        provider.keep_login(&login, &held)?;
        if let Err(error) = self.record_account(&login.session) {
            if stores_new_account {
                self.discard_unrecorded_credential(id);
            }
            return Err(error);
        }
        self.hold_session(login.session)
    }

    /// Deletes the credential a login just stored for an account that is not
    /// recorded. Runs under the credential order, so no other login can have
    /// stored one for it since.
    ///
    /// An account the configuration names after all keeps its credential.
    fn discard_unrecorded_credential(&mut self, id: Uuid) {
        let account = id.to_string();
        // Read without adopting it, so a failed login changes nothing else.
        match ephervex_storage::Config::load_or_default(self.paths()) {
            Ok(config) if !config.accounts.contains_key(&account) => {}
            Ok(_) => return,
            // It may well record the account, so the credential stays.
            Err(error) => {
                warn!(%error, account = %id, "kept the credential of a login whose record could not be checked");
                return;
            }
        }
        let deleted = self
            .keyring()
            .and_then(|store| store.delete(&CredentialKey::MicrosoftRefreshToken(id)));
        match deleted {
            Ok(_) => self.forget_cleanup_intent(&account),
            Err(error) => {
                warn!(%error, account = %id, "could not remove the credential of a login that was not recorded");
            }
        }
    }

    /// Drops the pending-cleanup entry a login wrote before storing, once its
    /// credential is gone or was never stored.
    fn forget_cleanup_intent(&mut self, account: &str) {
        let result = self.mutate_config(|config| config.keyring_cleanup.retain(|p| p != account));
        if let Err(error) = result {
            warn!(%error, account, "could not clear the pending cleanup of a login that was not recorded");
        }
    }

    /// Writes a completed session's account metadata to the configuration.
    fn record_account(&mut self, session: &Session) -> CoreResult<()> {
        self.write_account_metadata(session, |config, id, name, at| {
            config.remember_account(id, name, session.account.kind, at);
            true
        })
    }

    /// [`Self::record_account`] for a resume: refuses to bring back an
    /// account that is no longer recorded.
    fn refresh_recorded_account(&mut self, session: &Session) -> CoreResult<()> {
        self.write_account_metadata(session, |config, id, name, at| {
            config.refresh_account(id, name, session.account.kind, at)
        })
    }

    /// Applies `write` to the configuration under its lock, for a session that
    /// will still be here next time. `write` returns whether the account is
    /// recorded afterwards; when it is not, the session is signed out.
    fn write_account_metadata(
        &mut self,
        session: &Session,
        write: impl FnOnce(&mut ephervex_storage::Config, &str, &str, &str) -> bool,
    ) -> CoreResult<()> {
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
            let mut recorded = false;
            self.mutate_config(|config| recorded = write(config, &id, &name, &at))?;
            if !recorded {
                return Err(CoreError::Auth(AuthError::NotSignedIn));
            }
        } else {
            debug!(account = %id, "session-only login; nothing written to the configuration");
        }
        Ok(())
    }

    /// Keeps a recorded session in memory for launches.
    fn hold_session(&mut self, session: Session) -> CoreResult<&Session> {
        self.session = Some(session);
        self.session
            .as_ref()
            .ok_or(CoreError::Auth(AuthError::NotSignedIn))
    }
}

/// The keyring a test build uses instead of Secret Service.
///
/// A test build never opens the desktop keyring: a test that needs stored
/// credentials names the store, and every other test finds no secret storage.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) enum TestKeyring {
    #[default]
    Unreachable,
    Store(Arc<dyn CredentialStore>),
}

#[cfg(test)]
impl TestKeyring {
    pub(crate) fn open(&self) -> Result<Arc<dyn CredentialStore>, AuthError> {
        match self {
            Self::Store(store) => Ok(Arc::clone(store)),
            Self::Unreachable => Err(AuthError::SecretStorageUnavailable {
                detail: "no keyring in this test".to_owned(),
            }),
        }
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
    use super::*;

    fn app_rooted_at(base: &std::path::Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app opens")
    }

    fn remember_microsoft_account(app: &mut App, id: Uuid) {
        app.mutate_config(|config| {
            config.remember_account(
                &id.to_string(),
                "Steve",
                AccountKind::Microsoft,
                "2026-01-01T00:00:00Z",
            );
        })
        .expect("remember");
    }

    fn microsoft_session(id: Uuid) -> Session {
        Session {
            account: ephervex_auth::Account {
                id,
                name: "Steve".to_owned(),
                kind: AccountKind::Microsoft,
                authenticated_at: Utc::now(),
            },
            credentials: ephervex_auth::Credentials::Microsoft {
                token: SecretString::new("access".to_owned()),
                expires_at: Utc::now() + TimeDelta::hours(1),
                entitlements: ephervex_auth::EntitlementLookup::Answered(
                    ephervex_auth::Entitlements::default(),
                ),
                storage: CredentialStorage::Keyring,
            },
        }
    }

    fn keyring_holding(id: Uuid) -> Arc<MemoryStore> {
        let store = Arc::new(MemoryStore::new());
        store
            .store(
                &ephervex_auth::CredentialKey::MicrosoftRefreshToken(id),
                &SecretString::new("refresh".to_owned()),
            )
            .expect("store");
        store
    }

    /// Makes every configuration write fail while the file stays readable.
    fn block_configuration_writes(app: &App) {
        let lock = app.paths().config_dir().join("config.lock");
        let _ = std::fs::remove_file(&lock);
        std::fs::create_dir_all(lock).expect("block");
    }

    fn provider_over(store: Arc<dyn CredentialStore>) -> MicrosoftAccountProvider {
        MicrosoftAccountProvider::with_endpoints(
            ephervex_auth::resolve_client_id(""),
            store,
            ephervex_auth::Endpoints::production(),
            &HttpConfig::default(),
        )
        .expect("provider")
    }

    fn login_with(id: Uuid, refresh: &str) -> Login {
        Login::new(
            microsoft_session(id),
            Some(SecretString::new(refresh.to_owned())),
        )
    }

    fn refresh_key(id: Uuid) -> CredentialKey {
        CredentialKey::MicrosoftRefreshToken(id)
    }

    /// A keyring that runs a hook right after each store, as another process
    /// or a failure would between a login's store and its record.
    struct HookedStore {
        inner: MemoryStore,
        after_store: Box<dyn Fn() + Send + Sync>,
    }

    impl std::fmt::Debug for HookedStore {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("HookedStore")
        }
    }

    impl CredentialStore for HookedStore {
        fn store(&self, key: &CredentialKey, secret: &SecretString) -> AuthResult<()> {
            self.inner.store(key, secret)?;
            (self.after_store)();
            Ok(())
        }

        fn load(&self, key: &CredentialKey) -> AuthResult<Option<SecretString>> {
            self.inner.load(key)
        }

        fn delete(&self, key: &CredentialKey) -> AuthResult<CredentialRemoval> {
            self.inner.delete(key)
        }

        fn storage(&self) -> CredentialStorage {
            CredentialStorage::Keyring
        }
    }

    /// Runs `body` on a thread and reports whether it is still waiting after
    /// a grace period, then lets the caller release what it waits on.
    fn still_waiting<T: Send + 'static>(
        body: impl FnOnce() -> T + Send + 'static,
    ) -> (std::thread::JoinHandle<T>, bool) {
        let handle = std::thread::spawn(body);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let waiting = !handle.is_finished();
        (handle, waiting)
    }

    #[test]
    fn a_login_that_cannot_be_recorded_removes_the_credential_it_stored() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        let blocked = app.paths().config_dir().join("config.lock");
        let store = Arc::new(HookedStore {
            inner: MemoryStore::new(),
            // The intent was written before the store; recording fails after it.
            after_store: Box::new(move || {
                let _ = std::fs::remove_file(&blocked);
                std::fs::create_dir_all(&blocked).expect("block");
            }),
        });
        app.test_keyring = TestKeyring::Store(store.clone());
        let provider = provider_over(store.clone());

        app.adopt_login(&provider, login_with(id, "refresh"))
            .expect_err("the account cannot be recorded");

        assert!(store.load(&refresh_key(id)).expect("load").is_none());
        assert!(app.session().is_none());
    }

    #[test]
    fn a_failed_record_keeps_the_credential_of_an_account_already_recorded() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        remember_microsoft_account(&mut app, id);
        let store = keyring_holding(id);
        app.test_keyring = TestKeyring::Store(store.clone());
        block_configuration_writes(&app);

        app.adopt_login(&provider_over(store.clone()), login_with(id, "newer"))
            .expect_err("the account cannot be recorded again");

        let kept = store.load(&refresh_key(id)).expect("load");
        assert!(kept.is_some(), "the recorded account keeps a credential");
    }

    #[test]
    fn a_login_stores_nothing_when_the_configuration_cannot_be_read() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        let store = Arc::new(MemoryStore::new());
        app.test_keyring = TestKeyring::Store(store.clone());
        // A directory where the file belongs can neither be read nor written.
        std::fs::create_dir_all(app.paths().config_file()).expect("block the file");

        app.adopt_login(&provider_over(store.clone()), login_with(id, "refresh"))
            .expect_err("the account cannot be recorded");

        assert!(
            store.load(&refresh_key(id)).expect("load").is_none(),
            "no credential is stored for an account that cannot be recorded"
        );
    }

    #[test]
    fn a_login_that_stores_a_credential_is_covered_by_a_pending_cleanup_until_recorded() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        let seen_covered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let store = Arc::new(HookedStore {
            inner: MemoryStore::new(),
            after_store: Box::new({
                let seen_covered = Arc::clone(&seen_covered);
                let paths = Paths::rooted_at(base.path());
                move || {
                    let config = ephervex_storage::Config::load_or_default(&paths).expect("load");
                    seen_covered.store(
                        config.keyring_cleanup == [id.to_string()],
                        std::sync::atomic::Ordering::SeqCst,
                    );
                }
            }),
        });
        app.test_keyring = TestKeyring::Store(store.clone());

        app.adopt_login(&provider_over(store.clone()), login_with(id, "refresh"))
            .expect("recorded");

        assert!(
            seen_covered.load(std::sync::atomic::Ordering::SeqCst),
            "a login that dies after storing leaves a credential a cleanup deletes"
        );
        let config = app_rooted_at(base.path()).config().clone();
        assert!(config.keyring_cleanup.is_empty(), "recording clears it");
        assert!(config.accounts.contains_key(&id.to_string()));
        assert!(store.load(&refresh_key(id)).expect("load").is_some());
    }

    /// A keyring that cannot store, as one that is locked and unanswered.
    #[derive(Debug)]
    struct RefusingStore(MemoryStore);

    impl CredentialStore for RefusingStore {
        fn store(&self, _key: &CredentialKey, _secret: &SecretString) -> AuthResult<()> {
            Err(AuthError::SecretStorageUnavailable {
                detail: "locked".to_owned(),
            })
        }

        fn load(&self, key: &CredentialKey) -> AuthResult<Option<SecretString>> {
            self.0.load(key)
        }

        fn delete(&self, key: &CredentialKey) -> AuthResult<CredentialRemoval> {
            self.0.delete(key)
        }

        fn storage(&self) -> CredentialStorage {
            CredentialStorage::Keyring
        }
    }

    #[test]
    fn a_login_whose_store_fails_keeps_the_pending_cleanup_of_an_earlier_credential() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        // An earlier removal left its credential behind and its cleanup pending.
        let store = Arc::new(RefusingStore(MemoryStore::new()));
        store
            .0
            .store(&refresh_key(id), &SecretString::new("earlier".to_owned()))
            .expect("seed");
        app.mutate_config(|config| config.record_keyring_cleanup(&id.to_string()))
            .expect("pending cleanup");
        app.test_keyring = TestKeyring::Store(store.clone());

        app.adopt_login(&provider_over(store.clone()), login_with(id, "refresh"))
            .expect_err("the keyring refuses the credential");

        assert_eq!(
            app_rooted_at(base.path()).config().keyring_cleanup,
            [id.to_string()],
            "the earlier credential is still named by a pending cleanup"
        );
        assert!(store.load(&refresh_key(id)).expect("load").is_some());
    }

    #[test]
    fn a_credential_left_by_an_interrupted_login_is_deleted_by_the_next_cleanup() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        // What a login that died between its store and its record leaves.
        let store = keyring_holding(id);
        app.mutate_config(|config| config.record_keyring_cleanup(&id.to_string()))
            .expect("intent");
        app.test_keyring = TestKeyring::Store(store.clone());

        app.finish_keyring_cleanup();

        assert!(store.load(&refresh_key(id)).expect("load").is_none());
        assert!(app.config().keyring_cleanup.is_empty());
    }

    #[test]
    fn a_login_without_a_refresh_credential_leaves_nothing_to_clean_up() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        let store = Arc::new(MemoryStore::new());
        app.test_keyring = TestKeyring::Store(store.clone());
        block_configuration_writes(&app);

        let login = Login::new(microsoft_session(id), None);
        app.adopt_login(&provider_over(store.clone()), login)
            .expect_err("the account cannot be recorded");

        assert!(store.load(&refresh_key(id)).expect("load").is_none());
    }

    #[test]
    fn a_removal_waits_for_a_login_that_is_storing_and_then_removes_what_it_stored() {
        let base = tempfile::tempdir().expect("temp dir");
        let id = Uuid::new_v4();
        let (stored_tx, stored_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let store = Arc::new(HookedStore {
            inner: MemoryStore::new(),
            after_store: Box::new({
                let stored_tx = std::sync::Mutex::new(stored_tx);
                let release_rx = std::sync::Mutex::new(release_rx);
                move || {
                    stored_tx.lock().expect("lock").send(()).expect("signal");
                    release_rx.lock().expect("lock").recv().expect("release");
                }
            }),
        });

        let login = std::thread::spawn({
            let store = Arc::clone(&store);
            let root = base.path().to_path_buf();
            move || {
                let mut app = app_rooted_at(&root);
                app.test_keyring = TestKeyring::Store(store.clone());
                app.adopt_login(&provider_over(store), login_with(id, "refresh"))
                    .map(|_| ())
            }
        });
        stored_rx.recv().expect("the login stored its credential");

        // The account is not recorded yet, so only the order can protect the
        // credential from a removal of the same account.
        let (removal, waiting) = still_waiting({
            let store = Arc::clone(&store);
            let root = base.path().to_path_buf();
            move || {
                let mut app = app_rooted_at(&root);
                app.test_keyring = TestKeyring::Store(store);
                app.remove_account(&id.to_string()).map(|_| ())
            }
        });
        assert!(waiting, "the removal must wait for the login in progress");

        release_tx.send(()).expect("release");
        login.join().expect("login thread").expect("login");
        removal.join().expect("removal thread").expect("removed");

        // The removal ran after the login recorded the account, so neither remains.
        let config = app_rooted_at(base.path()).config().clone();
        assert!(!config.accounts.contains_key(&id.to_string()));
        assert!(store.load(&refresh_key(id)).expect("load").is_none());
    }

    #[test]
    fn a_removal_waits_for_a_credential_change_held_elsewhere() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        remember_microsoft_account(&mut app, id);
        let store = keyring_holding(id);
        let paths = app.paths().clone();
        let held = CredentialChangeLock::take(&paths).expect("lock");

        let (removal, waiting) = still_waiting({
            let store = Arc::clone(&store);
            let root = base.path().to_path_buf();
            move || {
                let mut app = app_rooted_at(&root);
                app.test_keyring = TestKeyring::Store(store);
                app.remove_account(&id.to_string()).map(|_| ())
            }
        });
        assert!(waiting, "a removal must not run while a credential changes");
        assert!(store.load(&refresh_key(id)).expect("load").is_some());

        drop(held);
        removal.join().expect("removal thread").expect("removed");
        assert!(store.load(&refresh_key(id)).expect("load").is_none());
    }

    #[test]
    fn a_pending_cleanup_does_not_delete_the_credential_of_a_login_recorded_since() {
        let base = tempfile::tempdir().expect("temp dir");
        let id = Uuid::new_v4();
        let mut stale = app_rooted_at(base.path());
        stale
            .mutate_config(|config| config.record_keyring_cleanup(&id.to_string()))
            .expect("record the earlier removal's cleanup");
        let store = Arc::new(MemoryStore::new());
        stale.test_keyring = TestKeyring::Store(store.clone());

        // Another window signs the same account in again, which clears the entry.
        let mut other = app_rooted_at(base.path());
        other.test_keyring = TestKeyring::Store(store.clone());
        other
            .adopt_login(&provider_over(store.clone()), login_with(id, "fresh"))
            .expect("recorded");

        stale.clean_up_keyring(store.as_ref());

        assert!(
            store.load(&refresh_key(id)).expect("load").is_some(),
            "the cleanup was decided against the list as it is now"
        );
    }

    #[test]
    fn a_login_waits_for_another_login_of_the_same_account_and_neither_deletes_the_other() {
        let base = tempfile::tempdir().expect("temp dir");
        let id = Uuid::new_v4();
        let store = Arc::new(MemoryStore::new());
        let paths = Paths::rooted_at(base.path());
        let held = CredentialChangeLock::take(&paths).expect("lock");

        let (second, waiting) = still_waiting({
            let store = Arc::clone(&store);
            let root = base.path().to_path_buf();
            move || {
                let mut app = app_rooted_at(&root);
                app.test_keyring = TestKeyring::Store(store.clone());
                app.adopt_login(&provider_over(store), login_with(id, "second"))
                    .map(|_| ())
            }
        });
        assert!(waiting, "a login must wait for the change in progress");

        // The login in progress elsewhere stores and records its account.
        let mut first = app_rooted_at(base.path());
        store
            .store(&refresh_key(id), &SecretString::new("first".to_owned()))
            .expect("store");
        remember_microsoft_account(&mut first, id);
        drop(held);

        second.join().expect("login thread").expect("login");
        let kept = store.load(&refresh_key(id)).expect("load");
        assert!(kept.is_some(), "the recorded account keeps its credential");
        assert!(
            app_rooted_at(base.path())
                .config()
                .accounts
                .contains_key(&id.to_string())
        );
    }

    #[test]
    fn a_resume_finishing_after_the_account_was_removed_does_not_record_it_again() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut resuming = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        remember_microsoft_account(&mut resuming, id);
        let store = keyring_holding(id);
        resuming.test_keyring = TestKeyring::Store(store.clone());

        // The removal runs in another `App` after the resume's last check.
        let mut removing = app_rooted_at(base.path());
        removing.test_keyring = TestKeyring::Store(store.clone());
        removing.remove_account(&id.to_string()).expect("removed");

        let error = resuming
            .adopt(microsoft_session(id))
            .expect_err("the account is gone");

        assert!(
            matches!(error, CoreError::Auth(AuthError::NotSignedIn)),
            "{error:?}"
        );
        let config = app_rooted_at(base.path()).config().clone();
        assert!(!config.accounts.contains_key(&id.to_string()));
        assert!(config.active_account().is_none());
        assert!(resuming.session().is_none());
    }

    #[test]
    fn a_resume_keeps_the_pending_cleanup_of_the_account_it_updates() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        remember_microsoft_account(&mut app, id);
        app.mutate_config(|config| config.record_keyring_cleanup(&id.to_string()))
            .expect("record");

        app.adopt(microsoft_session(id)).expect("adopted");

        assert_eq!(app.config().keyring_cleanup, [id.to_string()]);
    }

    #[test]
    fn removing_an_account_while_the_keyring_is_unreachable_records_its_credential_for_cleanup() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        remember_microsoft_account(&mut app, id);
        app.test_keyring = TestKeyring::Unreachable;

        app.remove_account(&id.to_string()).expect("removed");

        let config = app_rooted_at(base.path()).config().clone();
        assert!(!config.accounts.contains_key(&id.to_string()));
        assert_eq!(config.keyring_cleanup, [id.to_string()]);
    }

    #[test]
    fn removing_an_account_while_the_keyring_is_unreachable_reports_the_deletion_as_scheduled() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        remember_microsoft_account(&mut app, id);
        app.test_keyring = TestKeyring::Unreachable;

        let removal = app.remove_account(&id.to_string()).expect("removed");

        assert_eq!(removal.credential, CredentialOutcome::ScheduledForDeletion);
    }

    #[test]
    fn logging_out_while_the_keyring_is_unreachable_reports_the_deletion_as_scheduled() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        remember_microsoft_account(&mut app, id);
        app.test_keyring = TestKeyring::Unreachable;

        assert_eq!(
            app.logout().expect("logged out"),
            Some(CredentialOutcome::ScheduledForDeletion)
        );
    }

    #[test]
    fn logging_out_with_a_saved_credential_reports_it_deleted() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        remember_microsoft_account(&mut app, id);
        let store = keyring_holding(id);
        app.test_keyring = TestKeyring::Store(store.clone());

        assert_eq!(
            app.logout().expect("logged out"),
            Some(CredentialOutcome::Deleted)
        );
        assert!(store.load(&refresh_key(id)).expect("load").is_none());
    }

    #[test]
    fn removing_a_microsoft_account_with_no_keyring_entry_reports_no_credential() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        remember_microsoft_account(&mut app, id);
        app.test_keyring = TestKeyring::Store(Arc::new(MemoryStore::new()));

        let removal = app.remove_account(&id.to_string()).expect("removed");

        assert_eq!(removal.credential, CredentialOutcome::NoneStored);
        assert!(app.config().keyring_cleanup.is_empty());
    }

    #[test]
    fn removing_an_offline_account_reports_no_credential_even_when_the_keyring_is_unreachable() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("Alice").expect("added");
        app.test_keyring = TestKeyring::Unreachable;

        assert_eq!(
            app.logout().expect("logged out"),
            Some(CredentialOutcome::NoneStored)
        );
    }

    #[test]
    fn a_recorded_credential_is_deleted_once_the_keyring_is_reachable() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        remember_microsoft_account(&mut app, id);
        app.test_keyring = TestKeyring::Unreachable;
        app.remove_account(&id.to_string()).expect("removed");

        let store = Arc::new(MemoryStore::new());
        let key = ephervex_auth::CredentialKey::MicrosoftRefreshToken(id);
        store
            .store(&key, &SecretString::new("refresh".to_owned()))
            .expect("store");
        app.test_keyring = TestKeyring::Store(store.clone());
        app.finish_keyring_cleanup();

        assert!(store.load(&key).expect("load").is_none());
        assert!(
            app_rooted_at(base.path())
                .config()
                .keyring_cleanup
                .is_empty()
        );
    }

    #[test]
    fn removing_an_account_with_a_reachable_keyring_deletes_its_credential() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        remember_microsoft_account(&mut app, id);
        let store = Arc::new(MemoryStore::new());
        let key = ephervex_auth::CredentialKey::MicrosoftRefreshToken(id);
        store
            .store(&key, &SecretString::new("refresh".to_owned()))
            .expect("store");
        app.test_keyring = TestKeyring::Store(store.clone());

        let removal = app.remove_account(&id.to_string()).expect("removed");

        assert_eq!(removal.credential, CredentialOutcome::Deleted);
        assert!(store.load(&key).expect("load").is_none());
        assert!(app.config().keyring_cleanup.is_empty());
    }

    #[test]
    fn a_setting_changed_through_one_app_keeps_an_account_another_app_added() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut worker = app_rooted_at(base.path());
        let mut detached = app_rooted_at(base.path());

        detached.add_offline_account("Alice").expect("add");
        worker
            .set_show_linked_servers(true)
            .expect("change a setting");

        let fresh = app_rooted_at(base.path());
        let names: Vec<String> = fresh
            .accounts()
            .expect("accounts")
            .into_iter()
            .map(|account| account.name)
            .collect();
        assert_eq!(
            names,
            ["Alice"],
            "the account survives the other App's write"
        );
        assert!(fresh.config().display.show_linked_servers);
        assert_eq!(
            worker.accounts().expect("accounts").len(),
            1,
            "the writer now holds what it wrote"
        );
    }

    #[test]
    fn an_account_setter_acts_on_an_account_another_app_added() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut worker = app_rooted_at(base.path());
        let mut detached = app_rooted_at(base.path());

        detached.add_offline_account("Alice").expect("add");

        worker
            .set_default_account("Alice")
            .expect("rereads what another App wrote");
    }

    #[test]
    fn the_api_settings_report_the_build_key_on_a_machine_without_secret_storage() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.test_keyring = TestKeyring::Unreachable;

        let settings = app.api_settings().expect("settings");

        assert!(settings.credential_storage.is_none());
        assert_eq!(
            settings.curseforge_key,
            if ephervex_auth::DEFAULT_CURSEFORGE_API_KEY.is_empty() {
                CurseForgeKeyState::Missing
            } else {
                CurseForgeKeyState::BuildDefault
            }
        );
    }

    #[test]
    fn an_app_with_no_named_keyring_finds_no_secret_storage_in_every_credential_entry_point() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let unreachable = |error: CoreError| {
            matches!(
                error,
                CoreError::Auth(AuthError::SecretStorageUnavailable { ref detail })
                    if detail == "no keyring in this test"
            )
        };

        assert!(unreachable(
            app.set_curseforge_api_key(&SecretString::new("key".to_owned()))
                .expect_err("no keyring")
        ));
        assert!(unreachable(
            app.clear_curseforge_api_key().expect_err("no keyring")
        ));
        assert!(unreachable(
            app.build_account_provider(false)
                .map(|_| ())
                .expect_err("no keyring")
        ));
        assert!(
            app.api_settings()
                .expect("settings")
                .credential_storage
                .is_none()
        );
        assert!(
            app.account_status()
                .expect("status")
                .credential_storage
                .is_none()
        );
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
    fn every_download_manager_is_configured_with_the_effective_user_agent() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.set_custom_user_agent(Some("BugReportBot/2.0"))
            .expect("set");

        assert_eq!(app.download_config().http.user_agent, "BugReportBot/2.0");
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
        assert_eq!(
            account.credential,
            CredentialPresence::NotApplicable,
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

        let remaining = app.remove_account("Alice").expect("removed").remaining;
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
            .remaining
            .expect("default");
        assert_eq!(remaining.name, "Alice");
    }

    #[test]
    fn removing_the_last_account_leaves_none() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("Alice").expect("added");

        assert!(
            app.remove_account("Alice")
                .expect("removed")
                .remaining
                .is_none()
        );
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

    /// The point of launching as somebody else: it is one launch, not a new
    /// preference. A launcher that changed the default as a side effect would
    /// send the next launch somewhere the user never asked for.
    #[tokio::test]
    async fn a_session_for_a_chosen_account_leaves_the_default_alone() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let first = app.add_offline_account("Alice").expect("added");
        let second = app.add_offline_account("Bob").expect("added");
        assert!(
            app.account_summary(first.id).expect("summary").is_default,
            "the first account added is the default"
        );

        let session = app
            .session_for_account("Bob", &AuthObserver::silent(), &CancellationToken::new())
            .await
            .expect("a chosen offline account is always ready to launch");

        assert_eq!(session.account.id, second.id);
        assert_eq!(session.account.name, "Bob");
        assert!(!session.is_authenticated());
        assert!(
            app.account_summary(first.id).expect("summary").is_default,
            "choosing an account for one launch must not change the default"
        );
    }

    /// A cached session belongs to whichever account established it.
    #[tokio::test]
    async fn asking_for_another_account_does_not_return_the_cached_one() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("Alice").expect("added");
        let second = app.add_offline_account("Bob").expect("added");

        // Establishes and caches the default's session.
        let default = app
            .authenticated_session(&AuthObserver::silent(), &CancellationToken::new())
            .await
            .expect("session")
            .account
            .id;

        let chosen = app
            .session_for_account("Bob", &AuthObserver::silent(), &CancellationToken::new())
            .await
            .expect("session")
            .account
            .id;

        assert_ne!(default, chosen);
        assert_eq!(chosen, second.id);
    }

    async fn default_launch_account(app: &mut App) -> Uuid {
        app.authenticated_session(&AuthObserver::silent(), &CancellationToken::new())
            .await
            .expect("session")
            .account
            .id
    }

    #[tokio::test]
    async fn a_default_launch_after_launching_as_another_account_runs_as_the_default() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let default = app.add_offline_account("Alice").expect("added");
        app.add_offline_account("Bob").expect("added");

        app.session_for_account("Bob", &AuthObserver::silent(), &CancellationToken::new())
            .await
            .expect("session");

        assert_eq!(default_launch_account(&mut app).await, default.id);
    }

    #[tokio::test]
    async fn a_default_launch_after_a_non_default_sign_in_runs_as_the_default() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let default = app.add_offline_account("Alice").expect("added");
        app.hold_session(microsoft_session(Uuid::new_v4()))
            .expect("held");

        assert_eq!(default_launch_account(&mut app).await, default.id);
    }

    #[tokio::test]
    async fn a_default_launch_reuses_the_cached_session_of_the_default_account() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        remember_microsoft_account(&mut app, id);
        app.hold_session(microsoft_session(id)).expect("held");

        // The keyring is unreachable, so resuming would fail.
        assert_eq!(default_launch_account(&mut app).await, id);
    }

    #[tokio::test]
    async fn a_session_only_login_serves_default_launches_when_no_account_is_configured() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let id = Uuid::new_v4();
        let mut session = microsoft_session(id);
        if let ephervex_auth::Credentials::Microsoft { storage, .. } = &mut session.credentials {
            *storage = CredentialStorage::SessionOnly;
        }
        assert_eq!(session.storage(), Some(CredentialStorage::SessionOnly));
        app.hold_session(session).expect("held");

        assert_eq!(default_launch_account(&mut app).await, id);
    }

    #[tokio::test]
    async fn launching_as_an_account_that_does_not_exist_says_so() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        app.add_offline_account("Alice").expect("added");

        let error = app
            .session_for_account("Nobody", &AuthObserver::silent(), &CancellationToken::new())
            .await
            .expect_err("an unknown account cannot be launched as");
        assert!(
            matches!(
                error,
                CoreError::Auth(AuthError::AccountUnknown { ref reference }) if reference == "Nobody"
            ),
            "{error:?}"
        );
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
        assert_eq!(
            app.logout().expect("logout"),
            None,
            "nothing to sign out of"
        );
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

    fn assert_secret_storage_unavailable(error: &CoreError) {
        assert!(
            matches!(
                error,
                CoreError::Auth(AuthError::SecretStorageUnavailable { .. })
            ),
            "{error:?}"
        );
        assert!(
            !error.requires_interactive_login(),
            "a keyring outage is not a reason to sign in again"
        );
    }

    #[tokio::test]
    async fn resuming_a_recorded_account_while_the_keyring_is_unreachable_reports_the_outage() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        remember_microsoft_account(&mut app, Uuid::new_v4());
        app.test_keyring = TestKeyring::Unreachable;

        let error = app
            .resume_account(&AuthObserver::silent(), &CancellationToken::new())
            .await
            .expect_err("the keyring cannot be opened");

        assert_secret_storage_unavailable(&error);
    }

    #[tokio::test]
    async fn launching_a_recorded_account_while_the_keyring_is_unreachable_reports_the_outage() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        remember_microsoft_account(&mut app, Uuid::new_v4());
        app.test_keyring = TestKeyring::Unreachable;

        let error = app
            .authenticated_session(&AuthObserver::silent(), &CancellationToken::new())
            .await
            .expect_err("the keyring cannot be opened");

        assert_secret_storage_unavailable(&error);
    }

    #[tokio::test]
    async fn launching_as_a_named_account_while_the_keyring_is_unreachable_reports_the_outage() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        remember_microsoft_account(&mut app, Uuid::new_v4());
        app.test_keyring = TestKeyring::Unreachable;

        let error = app
            .session_for_account("Steve", &AuthObserver::silent(), &CancellationToken::new())
            .await
            .expect_err("the keyring cannot be opened");

        assert_secret_storage_unavailable(&error);
    }

    #[test]
    fn an_account_status_with_an_unreachable_keyring_does_not_claim_the_credential_is_missing() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        remember_microsoft_account(&mut app, Uuid::new_v4());
        app.test_keyring = TestKeyring::Unreachable;

        let status = app.account_status().expect("status");

        assert_eq!(status.accounts[0].credential, CredentialPresence::Unknown);
    }

    #[test]
    fn an_account_status_distinguishes_a_stored_credential_from_a_missing_one() {
        let base = tempfile::tempdir().expect("temp dir");
        let mut app = app_rooted_at(base.path());
        let saved = Uuid::new_v4();
        remember_microsoft_account(&mut app, saved);
        let store = Arc::new(MemoryStore::new());
        store
            .store(
                &refresh_key(saved),
                &SecretString::new("refresh".to_owned()),
            )
            .expect("store");
        app.test_keyring = TestKeyring::Store(store);
        let unsaved = Uuid::new_v4();
        app.mutate_config(|config| {
            config.remember_account(
                &unsaved.to_string(),
                "Alex",
                AccountKind::Microsoft,
                "2026-01-01T00:00:00Z",
            );
        })
        .expect("remember");
        app.add_offline_account("Notch").expect("added");

        let status = app.account_status().expect("status");
        let presence = |name: &str| {
            status
                .accounts
                .iter()
                .find(|account| account.name == name)
                .expect("listed")
                .credential
        };

        assert_eq!(presence("Steve"), CredentialPresence::Stored);
        assert_eq!(presence("Alex"), CredentialPresence::Missing);
        assert_eq!(presence("Notch"), CredentialPresence::NotApplicable);
    }

    #[test]
    fn the_default_login_options_refuse_to_degrade_without_being_asked() {
        // session-only login is offered, never assumed.
        assert!(!LoginOptions::default().allow_session_only);
    }
}
