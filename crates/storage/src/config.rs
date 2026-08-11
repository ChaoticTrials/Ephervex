//! The versioned `config.toml`.
//!
//! # Two rules this module exists to enforce
//!
//! **It is versioned.** `schema_version` is read before anything else. A file
//! written by a newer Ephervex is refused rather than partially understood,
//! because silently dropping settings a user deliberately set is worse than
//! saying "update the launcher".
//!
//! **It never holds a secret.** ADR 0004: the Microsoft refresh token and the
//! CurseForge API key live in the desktop keyring. This is a *bug reproduction*
//! tool whose users routinely attach their configuration to bug reports, so
//! that rule is enforced here rather than merely documented — [`Config::save`]
//! refuses to write a key whose name looks like a credential.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use ephervex_model::AccountKind;

use crate::paths::{PathError, Paths};

/// The configuration schema this build understands.
///
/// Bumping this means writing a migration for the previous shape, exactly as
/// with the database.
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

/// Key-name fragments that mark a value as a credential.
///
/// Matched case-insensitively against every key at every depth. The list is
/// deliberately blunt: a false positive costs a user one renamed key, while a
/// false negative writes an authentication token into a file that ends up in a
/// bug report (ADR 0004).
pub const SECRET_KEY_MARKERS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "api_key",
    "apikey",
    "credential",
    "private_key",
    "access_key",
];

/// Failures reading or writing the configuration file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file exists but could not be read.
    #[error("could not read the configuration file {path}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid TOML, or a value has the wrong type.
    #[error("the configuration file {path} is not valid TOML")]
    Invalid {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
    /// The file could not be written.
    #[error("could not write the configuration file {path}")]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The in-memory configuration could not be turned into TOML.
    #[error("could not serialize the configuration")]
    Serialization(#[from] Box<toml::ser::Error>),
    /// The file was written by a newer version of the launcher.
    #[error("configuration schema version {found} is newer than the supported {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
    /// A key that looks like a credential was about to be written to disk.
    #[error("refusing to write the credential-looking key `{key}` into the configuration file")]
    SecretRejected { key: String },
    /// The configuration directory could not be created.
    #[error(transparent)]
    Path(#[from] PathError),
}

impl ConfigError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::Unreadable { .. } => "error-config-unreadable",
            Self::Invalid { .. } | Self::Serialization(_) => "error-config-invalid",
            Self::Unwritable { .. } => "error-config-unwritable",
            Self::UnsupportedSchema { .. } => "error-config-unsupported-schema",
            Self::SecretRejected { .. } => "error-config-secret-rejected",
            Self::Path(error) => error.message_id(),
        }
    }
}

/// Global JVM and Java-runtime settings.
///
/// `Eq` is deliberately absent here and on the other tables that carry an
/// `unknown` catch-all: `toml::Value` holds floats, so equality is partial.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JavaConfig {
    /// Scan the system for installed runtimes before offering to download one.
    pub auto_detect: bool,
    /// Download a managed runtime when no compatible one is installed.
    pub auto_download: bool,
    pub min_memory_mib: u32,
    pub max_memory_mib: u32,
    /// the preferred Java path setting, or empty for `Auto...`.
    ///
    /// A string rather than an `Option<PathBuf>` because that is what the
    /// settings field holds, and because clearing the box has to be
    /// expressible in TOML without deleting the key.
    pub preferred_path: String,
    /// Extra JVM arguments applied to every instance unless overridden.
    pub jvm_args: Vec<String>,
    /// Keys this build does not know about. See [`Config::unknown`].
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl Default for JavaConfig {
    fn default() -> Self {
        Self {
            auto_detect: true,
            auto_download: true,
            min_memory_mib: 512,
            max_memory_mib: 4096,
            preferred_path: String::new(),
            jvm_args: Vec::new(),
            unknown: toml::Table::new(),
        }
    }
}

impl JavaConfig {
    /// The configured preferred runtime, or `None` for automatic selection.
    #[must_use]
    pub fn preferred_path(&self) -> Option<PathBuf> {
        let trimmed = self.preferred_path.trim();
        (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
    }
}

/// One managed Minecraft preference.
///
/// `enabled` is separate from `value` on purpose: a preference the launcher
/// does *not* manage must be left entirely alone in `options.txt`, which is a
/// different thing from managing it and happening to agree with the current
/// value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreferenceSetting {
    pub enabled: bool,
    pub value: String,
}

impl PreferenceSetting {
    #[must_use]
    pub fn managed(value: impl Into<String>) -> Self {
        Self {
            enabled: true,
            value: value.into(),
        }
    }
}

/// How a test server binds to the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAccess {
    /// `server-ip=127.0.0.1`. The default: these are local test servers.
    #[default]
    Localhost,
    /// Empty `server-ip`, i.e. bind all interfaces.
    Lan,
    /// An explicit, validated bind address chosen per server.
    Custom,
}

/// Defaults applied to newly created servers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerDefaults {
    /// Off by default. These servers exist to test mods, not to be public.
    pub online_mode: bool,
    pub network_access: NetworkAccess,
    pub port: u16,
    /// Keys this build does not know about. See [`Config::unknown`].
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl Default for ServerDefaults {
    fn default() -> Self {
        Self {
            online_mode: false,
            network_access: NetworkAccess::Localhost,
            port: 25565,
            unknown: toml::Table::new(),
        }
    }
}

/// Provider credentials and identifiers that are *not* secrets.
///
/// # What is deliberately absent
///
/// Three user-configurable integration settings are grouped under "APIs": the Microsoft Client ID, the
/// CurseForge API Key, and an advanced custom User-Agent. Two of them are here.
/// The API key is not, and its absence is the point — it is a credential, so it
/// lives in the desktop keyring under `CredentialKey::CurseForgeApiKey` and
/// this struct has no field it could be written to (ADR 0004).
///
/// A settings screen still shows all three together; only one of them is backed
/// by this file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ApiConfig {
    /// The Azure application registration to authenticate as, or empty for the
    /// one this build ships with.
    ///
    /// Not a secret: Ephervex is registered as a public/native client, which
    /// has no client secret and cannot have one. The Client ID identifies the
    /// application to Microsoft and authenticates nothing.
    pub microsoft_client_id: String,
    /// An advanced override for the HTTP `User-Agent`, or empty for the
    /// launcher's own.
    ///
    /// Exists because a provider may one day need a particular string to grant
    /// access; it is not something a user should normally touch.
    pub user_agent: String,
    /// Keys this build does not know about. See [`Config::unknown`].
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl ApiConfig {
    /// The configured Client ID, or `None` to use the build's default.
    #[must_use]
    pub fn microsoft_client_id(&self) -> Option<&str> {
        let trimmed = self.microsoft_client_id.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    /// The configured User-Agent, or `None` for the launcher's own.
    #[must_use]
    pub fn user_agent(&self) -> Option<&str> {
        let trimmed = self.user_agent.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }
}

/// What is remembered about a signed-in account between runs.
///
/// Every field here is explicitly permitted in configuration: the profile id,
/// the display name, and timestamps. The refresh token that makes the account
/// usable is in the keyring and is not represented here at all — not even as a
/// copy, and not even as a length.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AccountConfig {
    /// The account's Minecraft display name, for showing in the interface
    /// without a network round trip.
    pub name: String,
    /// Microsoft or offline.
    ///
    /// Absent in a file written before offline accounts existed, and
    /// [`AccountKind::default`] reads that as Microsoft — which is what such a
    /// row is.
    pub kind: AccountKind,
    /// RFC 3339 timestamp of when this account was first added.
    pub added_at: String,
    /// RFC 3339 timestamp of the last successful chain completion, or of
    /// creation for an offline account, which never authenticates.
    pub last_authenticated_at: String,
}

/// Why an account reference did not resolve to exactly one account.
///
/// Two outcomes rather than an `Option`, because they need different messages:
/// "no such account" is a typo, and "that name belongs to two accounts" needs
/// the user to disambiguate with a UUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountLookup {
    Unknown,
    Ambiguous { matches: usize },
}

/// Recorded Minecraft EULA workflow.
///
/// The launcher must never claim on the user's behalf that they accepted the
/// EULA, so this is stored explicitly and can be revoked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct EulaConfig {
    pub accepted: bool,
    /// RFC 3339 timestamp, or empty when never accepted.
    pub accepted_at: String,
    /// The EULA URL or document marker the user accepted.
    pub reference: String,
    /// Keys this build does not know about. See [`Config::unknown`].
    #[serde(flatten)]
    pub unknown: toml::Table,
}

/// The launcher configuration file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Checked before anything else; see [`CONFIG_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// BCP 47 language tag. A language setting exists even though only English
    /// ships.
    pub language: String,
    /// Custom persistent instance root, or empty for the XDG default. This is
    /// the only relocatable root.
    pub persistent_instance_root: String,
    /// The account launches use, as a profile UUID, or empty when nobody is
    /// signed in.
    ///
    /// Separate from [`Self::accounts`] so that "which one is active" is one
    /// value rather than a flag that two rows could both claim. One account
    /// ships, but the shape does not foreclose a second.
    pub active_account: String,
    pub java: JavaConfig,
    /// Managed Minecraft preferences, keyed by preference name. An open map
    /// rather than a fixed struct because the set of managed keys grows with
    /// the version-aware options adapters.
    pub minecraft_preferences: BTreeMap<String, PreferenceSetting>,
    pub server_defaults: ServerDefaults,
    pub eula: EulaConfig,
    pub api: ApiConfig,
    /// Non-secret metadata for accounts that have signed in, keyed by profile
    /// UUID.
    ///
    /// A map rather than a single `[account]` table because Ephervex supports
    /// one account ships, and the data model must not make a second one a
    /// schema change. Same arrangement as [`Self::minecraft_preferences`].
    pub accounts: BTreeMap<String, AccountConfig>,
    /// Top-level keys this build does not know about.
    ///
    /// Carried through a load/save cycle so that running an older Ephervex once
    /// does not quietly discard settings a newer one wrote. Secret-looking keys
    /// are the exception: they are dropped on load and refused on save.
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            language: "en-US".to_owned(),
            persistent_instance_root: String::new(),
            active_account: String::new(),
            java: JavaConfig::default(),
            minecraft_preferences: BTreeMap::from([
                ("gui_scale".to_owned(), PreferenceSetting::managed("auto")),
                ("language".to_owned(), PreferenceSetting::managed("en_us")),
            ]),
            server_defaults: ServerDefaults::default(),
            eula: EulaConfig::default(),
            api: ApiConfig::default(),
            accounts: BTreeMap::new(),
            unknown: toml::Table::new(),
        }
    }
}

impl Config {
    /// Loads the configuration, falling back to defaults when the file does not
    /// exist yet.
    ///
    /// A missing file is a first run, not an error, and nothing is written —
    /// the file appears when the user first changes a setting.
    pub fn load_or_default(paths: &Paths) -> Result<Self, ConfigError> {
        Self::load_from(&paths.config_file())
    }

    /// Loads from an explicit path.
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path = %path.display(), "no configuration file yet; using defaults");
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(ConfigError::Unreadable {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        Self::from_toml_str(&text).map_err(|error| match error {
            ConfigError::Invalid { source, .. } => ConfigError::Invalid {
                path: path.to_path_buf(),
                source,
            },
            other => other,
        })
    }

    /// Parses configuration text.
    pub fn from_toml_str(text: &str) -> Result<Self, ConfigError> {
        // The version gate runs against the raw document. Deserializing first
        // would mean a future file whose `[java]` table changed shape reports a
        // type error instead of the honest "written by a newer version".
        let document: toml::Table =
            toml::from_str(text).map_err(|source| ConfigError::Invalid {
                path: PathBuf::new(),
                source: Box::new(source),
            })?;

        let found = document
            .get("schema_version")
            .and_then(toml::Value::as_integer)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(CONFIG_SCHEMA_VERSION);
        if found > CONFIG_SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedSchema {
                found,
                supported: CONFIG_SCHEMA_VERSION,
            });
        }

        let mut config: Self =
            document
                .try_into()
                .map_err(|source: toml::de::Error| ConfigError::Invalid {
                    path: PathBuf::new(),
                    source: Box::new(source),
                })?;

        // A previous version, a hand edit, or a third-party tool may have left
        // a credential here. Drop it rather than carrying it back out to disk.
        config.strip_secret_looking_keys();
        Ok(config)
    }

    /// Serializes to TOML, refusing to emit anything that looks like a secret.
    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        if let Some(key) = self.find_secret_looking_key() {
            return Err(ConfigError::SecretRejected { key });
        }
        toml::to_string_pretty(self).map_err(|source| ConfigError::Serialization(Box::new(source)))
    }

    /// Writes the configuration to its XDG location, creating the directory.
    pub fn save(&self, paths: &Paths) -> Result<(), ConfigError> {
        crate::paths::create_dir_all(paths.config_dir())?;
        self.save_to(&paths.config_file())
    }

    /// Writes the configuration to an explicit path.
    ///
    /// Written to a sibling temporary file and renamed, so an interrupted save
    /// leaves the previous configuration intact instead of a truncated one.
    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        let text = self.to_toml_string()?;
        let temporary = path.with_extension("toml.tmp");

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| ConfigError::Unwritable {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|source| ConfigError::Unwritable {
                path: temporary.clone(),
                source,
            })?;
        std::fs::rename(&temporary, path).map_err(|source| ConfigError::Unwritable {
            path: path.to_path_buf(),
            source,
        })?;
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|source| ConfigError::Unwritable {
                    path: parent.to_path_buf(),
                    source,
                })?;
        }
        Ok(())
    }

    /// The configured persistent instance root, or `None` for the XDG default.
    #[must_use]
    pub fn persistent_instance_root(&self) -> Option<PathBuf> {
        let trimmed = self.persistent_instance_root.trim();
        (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
    }

    /// Applies the configured overrides to a path resolver.
    ///
    /// This is the single place configuration and paths meet, so no caller can
    /// read the override and forget to apply it.
    #[must_use]
    pub fn apply_to(&self, paths: Paths) -> Paths {
        paths.with_persistent_instance_root(self.persistent_instance_root())
    }

    /// The default account, or `None` when there is none.
    ///
    /// A string rather than a `Uuid` in the file itself, and validated here, so
    /// a hand-edited or truncated value degrades to "no default" rather than
    /// failing to parse the whole configuration. A pointer at an account that
    /// is not in the table is treated the same way.
    #[must_use]
    pub fn active_account(&self) -> Option<&str> {
        let trimmed = self.active_account.trim();
        (!trimmed.is_empty() && self.accounts.contains_key(trimmed)).then_some(trimmed)
    }

    /// The metadata recorded for the default account.
    #[must_use]
    pub fn active_account_config(&self) -> Option<(&str, &AccountConfig)> {
        let id = self.active_account()?;
        self.accounts.get(id).map(|account| (id, account))
    }

    /// Records an account, making it the default if there is not one already.
    ///
    /// Only non-secret metadata reaches this: the id, the name, the kind, and
    /// the times. Any credential went to the keyring before this was called,
    /// and an offline account has none at all.
    ///
    /// Adding does **not** steal the default from an account that already has
    /// it. Signing into a second account to check something should not silently
    /// change which one the next launch uses; [`Self::set_active_account`] is
    /// how that changes, and it is always an explicit act. The exception is the
    /// first account, which becomes the default because otherwise a single
    /// account would sit there not being used by anything.
    pub fn remember_account(&mut self, id: &str, name: &str, kind: AccountKind, at: &str) {
        let entry = self.accounts.entry(id.to_owned()).or_default();
        if entry.added_at.is_empty() {
            at.clone_into(&mut entry.added_at);
        }
        name.clone_into(&mut entry.name);
        entry.kind = kind;
        at.clone_into(&mut entry.last_authenticated_at);

        if self.active_account().is_none() {
            id.clone_into(&mut self.active_account);
        }
    }

    /// Makes an account the default.
    ///
    /// Returns `false` for an id this installation does not have, so a caller
    /// cannot point the default at nothing.
    pub fn set_active_account(&mut self, id: &str) -> bool {
        if !self.accounts.contains_key(id) {
            return false;
        }
        id.clone_into(&mut self.active_account);
        true
    }

    /// Forgets an account and, if it was the default, promotes another.
    ///
    /// The promotion rule is **the most recently used account that remains**,
    /// with the id as a tie-break so the outcome is deterministic rather than
    /// dependent on map ordering. That is the account the user was working with
    /// before this one, which is the least surprising thing to fall back to —
    /// and it beats leaving no default at all, which would make the next launch
    /// fail for a user who still has a perfectly good account.
    ///
    /// The stored credential is removed separately, by whoever owns the keyring
    /// — this crate has no business touching it (ADR 0004).
    pub fn forget_account(&mut self, id: &str) {
        self.accounts.remove(id);
        if self.active_account != id {
            return;
        }
        self.active_account = self
            .accounts
            .iter()
            .max_by(|(left_id, left), (right_id, right)| {
                left.last_authenticated_at
                    .cmp(&right.last_authenticated_at)
                    .then_with(|| left_id.cmp(right_id))
            })
            .map(|(id, _)| id.clone())
            .unwrap_or_default();
    }

    /// Finds an account by UUID or by display name.
    ///
    /// Names are matched case-insensitively because the user typed them, and
    /// an id is tried first because it is unambiguous by construction. A name
    /// held by more than one account resolves to nothing rather than to an
    /// arbitrary one — removing the wrong account is not recoverable.
    pub fn find_account(&self, reference: &str) -> Result<&str, AccountLookup> {
        let reference = reference.trim();
        if self.accounts.contains_key(reference) {
            return self
                .accounts
                .get_key_value(reference)
                .map(|(id, _)| id.as_str())
                .ok_or(AccountLookup::Unknown);
        }

        if let Ok(requested_id) = uuid::Uuid::parse_str(reference)
            && let Some(stored_id) = self.accounts.keys().find(|stored_id| {
                uuid::Uuid::parse_str(stored_id).is_ok_and(|candidate| candidate == requested_id)
            })
        {
            return Ok(stored_id);
        }

        let matches: Vec<&str> = self
            .accounts
            .iter()
            .filter(|(_, account)| account.name.eq_ignore_ascii_case(reference))
            .map(|(id, _)| id.as_str())
            .collect();

        match matches.as_slice() {
            [only] => Ok(only),
            [] => Err(AccountLookup::Unknown),
            many => Err(AccountLookup::Ambiguous {
                matches: many.len(),
            }),
        }
    }

    /// Whether any account has this name, for refusing a duplicate offline one.
    #[must_use]
    pub fn has_account_named(&self, name: &str) -> bool {
        self.accounts
            .values()
            .any(|account| account.name.eq_ignore_ascii_case(name.trim()))
    }

    /// Returns the dotted path of the first credential-looking key, if any.
    #[must_use]
    pub fn find_secret_looking_key(&self) -> Option<String> {
        // Only the open parts of the schema can hold an arbitrary key; every
        // named field in this module was chosen deliberately and holds no
        // secret. Checking them anyway would flag a future `api_key_present`
        // boolean, which is exactly the non-secret metadata ADR 0004 allows.
        find_secret_key(&self.unknown, "")
            .or_else(|| find_secret_key(&self.java.unknown, "java"))
            .or_else(|| find_secret_key(&self.server_defaults.unknown, "server_defaults"))
            .or_else(|| find_secret_key(&self.eula.unknown, "eula"))
            // `[api]` is the table a hand edit is most likely to put a
            // CurseForge key into, since that is where the settings screen
            // shows the field.
            .or_else(|| find_secret_key(&self.api.unknown, "api"))
    }

    fn strip_secret_looking_keys(&mut self) {
        for (table, prefix) in [
            (&mut self.unknown, ""),
            (&mut self.java.unknown, "java"),
            (&mut self.server_defaults.unknown, "server_defaults"),
            (&mut self.eula.unknown, "eula"),
            (&mut self.api.unknown, "api"),
        ] {
            while let Some(key) = find_secret_key(table, prefix) {
                let leaf = key.rsplit('.').next().unwrap_or(&key).to_owned();
                // Never log the value, only that a key was discarded.
                tracing::warn!(
                    key = %key,
                    "discarding a credential-looking key from the configuration file; \
                     secrets belong in the system keyring"
                );
                remove_nested(table, &leaf);
            }
        }
    }
}

/// Finds the first key at any depth whose name matches [`SECRET_KEY_MARKERS`].
fn find_secret_key(table: &toml::Table, prefix: &str) -> Option<String> {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let lowered = key.to_ascii_lowercase();
        if SECRET_KEY_MARKERS
            .iter()
            .any(|marker| lowered.contains(marker))
        {
            return Some(path);
        }
        if let toml::Value::Table(nested) = value {
            if let Some(found) = find_secret_key(nested, &path) {
                return Some(found);
            }
        }
    }
    None
}

/// Removes a key by leaf name at any depth, pruning tables that become empty.
fn remove_nested(table: &mut toml::Table, leaf: &str) {
    if table.remove(leaf).is_some() {
        return;
    }
    for (_, value) in table.iter_mut() {
        if let toml::Value::Table(nested) = value {
            remove_nested(nested, leaf);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::AccountKind;

    use super::*;

    /// The serialized configuration schema.
    const DEFAULT_CONFIG_EXAMPLE: &str = r#"
schema_version = 1
language = "en-US"
persistent_instance_root = ""

[java]
auto_detect = true
auto_download = true
min_memory_mib = 512
max_memory_mib = 4096
jvm_args = []

[minecraft_preferences.gui_scale]
enabled = true
value = "auto"

[minecraft_preferences.language]
enabled = true
value = "en_us"

[server_defaults]
online_mode = false
network_access = "localhost"
port = 25565

[eula]
accepted = false
accepted_at = ""
reference = ""
"#;

    #[test]
    fn the_default_schema_parses_into_exactly_the_defaults() {
        let parsed = Config::from_toml_str(DEFAULT_CONFIG_EXAMPLE)
            .expect("the default configuration example must parse");
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn defaults_match_the_product_decisions_they_encode() {
        let config = Config::default();
        assert_eq!(config.schema_version, CONFIG_SCHEMA_VERSION);
        assert_eq!(config.language, "en-US");
        // Test servers are for testing mods, not for being reachable.
        assert!(!config.server_defaults.online_mode);
        assert_eq!(
            config.server_defaults.network_access,
            NetworkAccess::Localhost
        );
        assert_eq!(config.server_defaults.port, 25565);
        // The launcher never pre-accepts the EULA on the user's behalf.
        assert!(!config.eula.accepted);
        assert!(config.java.auto_detect);
    }

    #[test]
    fn configuration_round_trips_through_toml() {
        let mut config = Config {
            persistent_instance_root: "/mnt/big/instances".to_owned(),
            ..Config::default()
        };
        config.java.max_memory_mib = 8192;
        config.java.jvm_args = vec!["-XX:+UseZGC".to_owned()];
        config.server_defaults.network_access = NetworkAccess::Lan;
        config.eula.accepted = true;
        config.eula.reference = "https://aka.ms/MinecraftEULA".to_owned();
        config
            .minecraft_preferences
            .insert("fullscreen".to_owned(), PreferenceSetting::managed("false"));

        let text = config.to_toml_string().expect("serialize");
        let parsed = Config::from_toml_str(&text).expect("re-parse");
        assert_eq!(parsed, config);
    }

    #[test]
    fn a_missing_file_yields_defaults_without_creating_one() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("config.toml");

        assert_eq!(
            Config::load_from(&path).expect("defaults"),
            Config::default()
        );
        assert!(!path.exists(), "loading must not create the file");
    }

    #[test]
    fn saving_and_loading_returns_the_same_configuration() {
        let directory = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(directory.path());

        let config = Config {
            persistent_instance_root: "/mnt/big/instances".to_owned(),
            ..Config::default()
        };
        config.save(&paths).expect("save");

        assert!(paths.config_file().is_file());
        assert_eq!(Config::load_or_default(&paths).expect("load"), config);

        // The temporary file used for the atomic rename must not survive.
        assert!(!paths.config_file().with_extension("toml.tmp").exists());
    }

    #[test]
    fn unknown_keys_survive_a_load_and_save_cycle() {
        // Running an older Ephervex once must not silently delete settings a
        // newer one wrote.
        // Prepended, not appended: anything after `[eula]` would land *inside*
        // that table rather than at the top level.
        let text = format!(
            "future_top_level = \"keep me\"\n{DEFAULT_CONFIG_EXAMPLE}\n[future_table]\nnested = 7\n"
        );
        let config = Config::from_toml_str(&text).expect("parse");
        assert_eq!(
            config
                .unknown
                .get("future_top_level")
                .and_then(toml::Value::as_str),
            Some("keep me")
        );

        let rewritten = config.to_toml_string().expect("serialize");
        assert!(rewritten.contains("future_top_level"));
        assert!(rewritten.contains("nested"));
        assert_eq!(Config::from_toml_str(&rewritten).expect("re-parse"), config);
    }

    #[test]
    fn unknown_keys_inside_known_tables_also_survive() {
        let text = DEFAULT_CONFIG_EXAMPLE
            .replace("jvm_args = []", "jvm_args = []\nfuture_java_option = \"x\"");
        let config = Config::from_toml_str(&text).expect("parse");
        assert_eq!(
            config
                .java
                .unknown
                .get("future_java_option")
                .and_then(toml::Value::as_str),
            Some("x")
        );
        assert!(
            config
                .to_toml_string()
                .expect("serialize")
                .contains("future_java_option")
        );
    }

    #[test]
    fn a_newer_schema_version_is_refused_rather_than_partially_understood() {
        let text = DEFAULT_CONFIG_EXAMPLE.replace("schema_version = 1", "schema_version = 99");
        let error = Config::from_toml_str(&text).expect_err("must refuse");
        assert!(matches!(
            error,
            ConfigError::UnsupportedSchema {
                found: 99,
                supported: 1
            }
        ));
        assert_eq!(error.message_id(), "error-config-unsupported-schema");
    }

    #[test]
    fn invalid_toml_reports_the_offending_path() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("config.toml");
        std::fs::write(&path, "this is not = = toml").expect("write");

        let error = Config::load_from(&path).expect_err("must fail");
        match &error {
            ConfigError::Invalid { path: reported, .. } => assert_eq!(reported, &path),
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(error.message_id(), "error-config-invalid");
    }

    #[test]
    fn no_secret_is_ever_stored_in_the_configuration_file() {
        // ADR 0004. This is the storage and registry support required behavior "no secret is
        // stored in config", enforced rather than merely asserted.
        let directory = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(directory.path());

        let mut config = Config::default();
        config.save(&paths).expect("save");
        let written = std::fs::read_to_string(paths.config_file()).expect("read back");
        let lowered = written.to_ascii_lowercase();
        for marker in SECRET_KEY_MARKERS {
            assert!(
                !lowered.contains(marker),
                "the default configuration must not mention `{marker}`"
            );
        }

        // A credential planted in the open part of the schema is refused.
        config.unknown.insert(
            "curseforge_api_key".to_owned(),
            toml::Value::String("$2a$10$notarealkey".to_owned()),
        );
        let error = config
            .save(&paths)
            .expect_err("must refuse to write a credential");
        assert!(matches!(error, ConfigError::SecretRejected { .. }));
        assert_eq!(error.message_id(), "error-config-secret-rejected");

        //...and the file on disk is untouched by the refused save.
        assert_eq!(
            std::fs::read_to_string(paths.config_file()).expect("read back"),
            written
        );
    }

    #[test]
    fn a_credential_found_in_an_existing_file_is_dropped_on_load() {
        let text = format!(
            "microsoft_refresh_token = \"M.C123_BAY.0.U.abc\"\n{DEFAULT_CONFIG_EXAMPLE}\n\
             [account]\naccess_token = \"eyJhbGciOi\"\nuuid = \"1234\"\n"
        );
        let config = Config::from_toml_str(&text).expect("parse");

        assert!(config.find_secret_looking_key().is_none());
        let rewritten = config.to_toml_string().expect("serialize");
        assert!(!rewritten.contains("M.C123_BAY"));
        assert!(!rewritten.contains("eyJhbGciOi"));
        assert!(!rewritten.to_ascii_lowercase().contains("token"));

        // Non-secret account metadata is explicitly allowed to stay (ADR 0004).
        assert!(rewritten.contains("uuid"));
    }

    #[test]
    fn the_persistent_instance_root_override_reaches_the_path_resolver() {
        let mut config = Config::default();
        assert!(config.persistent_instance_root().is_none());

        config.persistent_instance_root = "  /mnt/big/instances  ".to_owned();
        assert_eq!(
            config.persistent_instance_root(),
            Some(PathBuf::from("/mnt/big/instances"))
        );

        let paths = config.apply_to(Paths::rooted_at("/example"));
        assert_eq!(
            paths.persistent_instances_dir(),
            Path::new("/mnt/big/instances")
        );
        // The other roots stay where XDG says.
        assert_eq!(paths.config_dir(), Path::new("/example/config"));
    }

    #[test]
    fn the_api_settings_default_to_using_the_builds_own_values() {
        // An empty field in a settings screen means "use the default", not
        // "authenticate as nobody" and not "send an empty User-Agent".
        let api = ApiConfig::default();
        assert!(api.microsoft_client_id().is_none());
        assert!(api.user_agent().is_none());

        let configured = ApiConfig {
            microsoft_client_id: "  11112222-bbbb-3333-cccc-4444dddd5555  ".to_owned(),
            user_agent: " CustomAgent/1.0 ".to_owned(),
            unknown: toml::Table::new(),
        };
        assert_eq!(
            configured.microsoft_client_id(),
            Some("11112222-bbbb-3333-cccc-4444dddd5555")
        );
        assert_eq!(configured.user_agent(), Some("CustomAgent/1.0"));
    }

    #[test]
    fn the_api_table_has_no_field_a_curseforge_key_could_be_written_to() {
        // ADR 0004 requires the key to remain in the system keyring. This is enforced
        // by there being nowhere here to put it, not by remembering not to.
        let text = Config::default().to_toml_string().expect("serialize");
        let api_section = text
            .split("[api]")
            .nth(1)
            .expect("the api table is emitted");
        let lowered = api_section.to_ascii_lowercase();
        for marker in SECRET_KEY_MARKERS {
            assert!(!lowered.contains(marker), "`{marker}` in [api]:\n{text}");
        }
    }

    #[test]
    fn a_credential_hand_edited_into_the_api_table_is_dropped_on_load() {
        // The likeliest place for someone to put one, because it is where the
        // settings screen shows the field.
        let text =
            format!("{DEFAULT_CONFIG_EXAMPLE}\n[api]\ncurseforge_api_key = \"$2a$10$notreal\"\n");
        let config = Config::from_toml_str(&text).expect("parse");
        assert!(config.find_secret_looking_key().is_none());
        assert!(
            !config
                .to_toml_string()
                .expect("serialize")
                .contains("notreal")
        );
    }

    fn microsoft(config: &mut Config, id: &str, name: &str, at: &str) {
        config.remember_account(id, name, AccountKind::Microsoft, at);
    }

    #[test]
    fn account_metadata_round_trips_and_holds_nothing_secret() {
        let mut config = Config::default();
        assert!(config.active_account().is_none());

        microsoft(
            &mut config,
            "069a79f4-44e9-4726-a5be-fca90e38aaf5",
            "TestAccount",
            "2026-08-08T12:00:00Z",
        );
        let text = config.to_toml_string().expect("serialize");
        let parsed = Config::from_toml_str(&text).expect("re-parse");
        assert_eq!(parsed, config);

        let (id, account) = parsed.active_account_config().expect("an active account");
        assert_eq!(id, "069a79f4-44e9-4726-a5be-fca90e38aaf5");
        assert_eq!(account.name, "TestAccount");
        assert_eq!(account.kind, AccountKind::Microsoft);
        assert_eq!(account.added_at, "2026-08-08T12:00:00Z");
        assert_eq!(account.last_authenticated_at, "2026-08-08T12:00:00Z");

        // The id, name, kind, and timestamps are non-secret account metadata; only
        // nothing else. The refresh token is in the keyring and has no
        // representation in this file at all.
        let lowered = text.to_ascii_lowercase();
        for marker in SECRET_KEY_MARKERS {
            assert!(!lowered.contains(marker), "`{marker}`:\n{text}");
        }
    }

    #[test]
    fn an_offline_account_records_its_kind_and_still_holds_no_secret() {
        let mut config = Config::default();
        config.remember_account(
            "b50ad385-829d-3141-a216-7e7d7539ba7f",
            "Notch",
            AccountKind::Offline,
            "2026-08-08T12:00:00Z",
        );

        let text = config.to_toml_string().expect("serialize");
        assert!(text.contains("offline"), "{text}");
        let parsed = Config::from_toml_str(&text).expect("re-parse");
        let (_, account) = parsed.active_account_config().expect("active");
        assert_eq!(account.kind, AccountKind::Offline);

        let lowered = text.to_ascii_lowercase();
        for marker in SECRET_KEY_MARKERS {
            assert!(!lowered.contains(marker), "`{marker}`:\n{text}");
        }
    }

    #[test]
    fn an_account_row_without_a_kind_reads_as_microsoft() {
        // Written by a build that predates offline accounts. Reading it as
        // offline would silently downgrade a real account.
        let text = format!(
            "{DEFAULT_CONFIG_EXAMPLE}\n[accounts.069a79f4-44e9-4726-a5be-fca90e38aaf5]\n\
             name = \"TestAccount\"\nadded_at = \"\"\nlast_authenticated_at = \"\"\n"
        );
        let config = Config::from_toml_str(&text).expect("parse");
        assert_eq!(
            config.accounts["069a79f4-44e9-4726-a5be-fca90e38aaf5"].kind,
            AccountKind::Microsoft
        );
    }

    #[test]
    fn signing_in_again_keeps_the_original_added_at_and_updates_the_rest() {
        let mut config = Config::default();
        microsoft(&mut config, "id-1", "OldName", "2026-01-01T00:00:00Z");
        microsoft(&mut config, "id-1", "NewName", "2026-08-08T12:00:00Z");

        let (_, account) = config.active_account_config().expect("active");
        assert_eq!(account.added_at, "2026-01-01T00:00:00Z");
        assert_eq!(account.last_authenticated_at, "2026-08-08T12:00:00Z");
        assert_eq!(account.name, "NewName");
    }

    #[test]
    fn the_first_account_added_becomes_the_default_on_its_own() {
        // With one account there is nothing to choose, and leaving the default
        // empty would mean a launcher that has an account and refuses to use it.
        let mut config = Config::default();
        microsoft(&mut config, "id-1", "First", "2026-01-01T00:00:00Z");
        assert_eq!(config.active_account(), Some("id-1"));
    }

    #[test]
    fn adding_a_second_account_does_not_steal_the_default() {
        // Signing into another account to check something must not silently
        // change which one the next launch uses.
        let mut config = Config::default();
        microsoft(&mut config, "id-1", "First", "2026-01-01T00:00:00Z");
        microsoft(&mut config, "id-2", "Second", "2026-08-08T12:00:00Z");
        assert_eq!(config.active_account(), Some("id-1"));

        //...but asking for it does.
        assert!(config.set_active_account("id-2"));
        assert_eq!(config.active_account(), Some("id-2"));
    }

    #[test]
    fn the_default_cannot_be_pointed_at_an_account_that_does_not_exist() {
        let mut config = Config::default();
        microsoft(&mut config, "id-1", "First", "2026-01-01T00:00:00Z");
        assert!(!config.set_active_account("id-nope"));
        assert_eq!(config.active_account(), Some("id-1"));
    }

    #[test]
    fn removing_the_default_promotes_the_most_recently_used_survivor() {
        let mut config = Config::default();
        microsoft(&mut config, "id-old", "Old", "2026-01-01T00:00:00Z");
        microsoft(&mut config, "id-recent", "Recent", "2026-07-01T00:00:00Z");
        microsoft(&mut config, "id-active", "Active", "2026-08-08T12:00:00Z");
        assert!(config.set_active_account("id-active"));

        config.forget_account("id-active");
        assert_eq!(
            config.active_account(),
            Some("id-recent"),
            "the account used before this one is the least surprising fallback"
        );
    }

    #[test]
    fn removing_a_non_default_account_leaves_the_default_alone() {
        let mut config = Config::default();
        microsoft(&mut config, "id-1", "First", "2026-01-01T00:00:00Z");
        microsoft(&mut config, "id-2", "Second", "2026-08-08T12:00:00Z");

        config.forget_account("id-2");
        assert_eq!(config.active_account(), Some("id-1"));
        assert_eq!(config.accounts.len(), 1);
    }

    #[test]
    fn removing_the_last_account_leaves_no_default() {
        let mut config = Config::default();
        microsoft(&mut config, "id-1", "Only", "2026-01-01T00:00:00Z");
        config.forget_account("id-1");

        assert!(config.active_account().is_none());
        assert!(config.accounts.is_empty());
        assert!(config.active_account.is_empty());
    }

    #[test]
    fn promotion_is_deterministic_when_two_accounts_tie() {
        // Equal timestamps must not make the outcome depend on map ordering.
        let mut config = Config::default();
        microsoft(&mut config, "id-b", "B", "2026-01-01T00:00:00Z");
        microsoft(&mut config, "id-a", "A", "2026-01-01T00:00:00Z");
        microsoft(&mut config, "id-z", "Z", "2026-08-08T12:00:00Z");
        assert!(config.set_active_account("id-z"));

        config.forget_account("id-z");
        assert_eq!(config.active_account(), Some("id-b"), "id tie-break");
    }

    #[test]
    fn an_active_account_pointing_at_nothing_reads_as_no_default() {
        // A hand edit, or a configuration copied between machines, must not
        // make the launcher claim a login it has no metadata for.
        let config = Config {
            active_account: "069a79f4-44e9-4726-a5be-fca90e38aaf5".to_owned(),
            ..Config::default()
        };
        assert!(config.active_account().is_none());
        assert!(config.active_account_config().is_none());
    }

    #[test]
    fn accounts_resolve_by_uuid_or_by_name() {
        let mut config = Config::default();
        microsoft(
            &mut config,
            "069a79f4-44e9-4726-a5be-fca90e38aaf5",
            "Notch",
            "t",
        );
        config.remember_account(
            "b50ad385-829d-3141-a216-7e7d7539ba7f",
            "Steve",
            AccountKind::Offline,
            "t",
        );

        assert_eq!(
            config.find_account("069a79f4-44e9-4726-a5be-fca90e38aaf5"),
            Ok("069a79f4-44e9-4726-a5be-fca90e38aaf5")
        );
        assert_eq!(
            config.find_account("Steve"),
            Ok("b50ad385-829d-3141-a216-7e7d7539ba7f")
        );
        // The user typed it, so case is not their problem.
        assert_eq!(
            config.find_account("  notch  "),
            Ok("069a79f4-44e9-4726-a5be-fca90e38aaf5")
        );
        assert_eq!(config.find_account("nobody"), Err(AccountLookup::Unknown));

        config.remember_account(
            "8667ba71b85a4004af54457a9734eed7",
            "Compact",
            AccountKind::Microsoft,
            "t",
        );
        assert_eq!(
            config.find_account("8667ba71-b85a-4004-af54-457a9734eed7"),
            Ok("8667ba71b85a4004af54457a9734eed7")
        );
    }

    #[test]
    fn an_ambiguous_name_resolves_to_nothing_rather_than_to_a_guess() {
        // A Microsoft account and an offline account may legitimately share a
        // name. Removing the wrong one is not recoverable, so the user is made
        // to disambiguate.
        let mut config = Config::default();
        microsoft(&mut config, "id-1", "Notch", "t");
        config.remember_account("id-2", "Notch", AccountKind::Offline, "t");

        assert_eq!(
            config.find_account("Notch"),
            Err(AccountLookup::Ambiguous { matches: 2 })
        );
        // The unambiguous id still works.
        assert_eq!(config.find_account("id-2"), Ok("id-2"));
    }

    #[test]
    fn a_second_account_is_expressible_and_round_trips() {
        let mut config = Config::default();
        microsoft(&mut config, "id-1", "First", "2026-01-01T00:00:00Z");
        config.remember_account(
            "id-2",
            "Second",
            AccountKind::Offline,
            "2026-08-08T12:00:00Z",
        );

        assert_eq!(config.accounts.len(), 2);
        assert!(config.has_account_named("second"));
        assert!(!config.has_account_named("third"));
        let parsed =
            Config::from_toml_str(&config.to_toml_string().expect("serialize")).expect("parse");
        assert_eq!(parsed, config);
    }

    #[test]
    fn preferences_distinguish_unmanaged_from_managed_with_a_matching_value() {
        // An unmanaged key must be left alone in options.txt entirely, which is
        // not the same as managing it to its current value.
        let mut config = Config::default();
        let gui_scale = config
            .minecraft_preferences
            .get_mut("gui_scale")
            .expect("shipped by default");
        assert!(gui_scale.enabled);
        gui_scale.enabled = false;

        let text = config.to_toml_string().expect("serialize");
        let parsed = Config::from_toml_str(&text).expect("re-parse");
        assert!(!parsed.minecraft_preferences["gui_scale"].enabled);
        assert_eq!(parsed.minecraft_preferences["gui_scale"].value, "auto");
    }
}
