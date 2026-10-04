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
//! that rule is enforced here rather than merely documented - [`Config::save`]
//! refuses to write a key whose name looks like a credential.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use ephervex_model::{
    AccountKind, ArtifactCacheLimit, DEFAULT_ARTIFACT_CACHE_LIMIT_BYTES, DefaultModCatalog,
    ModProvider, OperatorPermissionLevel,
};

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

/// Location-only TOML diagnostics.
///
/// The upstream parser error retains and renders the complete offending input
/// line. Configuration is credential-adjacent, so carrying that error through
/// CLI or GUI source chains could disclose a malformed secret before the
/// normal key filter gets a chance to remove it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigParseError {
    line: Option<usize>,
    column: Option<usize>,
}

impl std::fmt::Display for ConfigParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.line, self.column) {
            (Some(line), Some(column)) => {
                write!(
                    formatter,
                    "TOML parse error at line {line}, column {column}"
                )
            }
            _ => formatter.write_str("TOML parse error"),
        }
    }
}

impl std::error::Error for ConfigParseError {}

/// The file in the configuration directory whose `flock` serializes updates.
const CONFIG_LOCK_FILE: &str = "config.lock";

/// How long an update waits on another process's before giving up, so a
/// stopped or wedged one cannot hang this one.
const CONFIG_LOCK_PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

/// Held while one update reloads, changes, and writes the configuration.
///
/// A `flock`, which the kernel drops if the holder dies, plus a process-wide
/// mutex, because NFS gives all threads of a process one shared lock.
struct ConfigWriteLock {
    _file: Option<std::fs::File>,
    _process: std::sync::MutexGuard<'static, ()>,
}

static CONFIG_WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl ConfigWriteLock {
    fn take(path: &Path) -> Result<Self, ConfigError> {
        use rustix::fs::{FlockOperation, Mode, OFlags, flock};
        use rustix::io::Errno;

        let deadline = std::time::Instant::now() + CONFIG_LOCK_PATIENCE;
        let busy = || ConfigError::Busy {
            path: path.to_path_buf(),
        };
        let wait = || {
            if std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
                Ok(())
            } else {
                Err(busy())
            }
        };
        let process = loop {
            match CONFIG_WRITES.try_lock() {
                Ok(held) => break held,
                // The mutex guards no data, so a panic while it was held leaves nothing torn.
                Err(std::sync::TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => wait()?,
            }
        };
        let unwritable = |errno: Errno| ConfigError::Unwritable {
            path: path.to_path_buf(),
            source: errno.into(),
        };
        let file = rustix::fs::open(
            path,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map(std::fs::File::from)
        .map_err(unwritable)?;
        loop {
            match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {
                    return Ok(Self {
                        _file: Some(file),
                        _process: process,
                    });
                }
                Err(Errno::INTR) => {}
                Err(Errno::WOULDBLOCK) => wait()?,
                // Without locking, updates within this process stay serialized.
                Err(errno @ (Errno::NOLCK | Errno::OPNOTSUPP | Errno::NOSYS)) => {
                    tracing::warn!(path = %path.display(), %errno, "configuration updates are not locked across processes here");
                    return Ok(Self {
                        _file: None,
                        _process: process,
                    });
                }
                Err(errno) => return Err(unwritable(errno)),
            }
        }
    }
}

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
        source: ConfigParseError,
    },
    /// The file could not be written.
    #[error("could not write the configuration file {path}")]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Another process kept the configuration locked for longer than an
    /// update waits.
    #[error("another process is updating the configuration ({path})")]
    Busy { path: PathBuf },
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
            Self::Busy { .. } => "error-config-busy",
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

/// Ephervex interface text scaling, independent of Minecraft's GUI scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationTextScale {
    Compact,
    #[default]
    Default,
    Comfortable,
    Large,
}

impl ApplicationTextScale {
    /// The percentage applied to Ephervex-owned interface typography.
    #[must_use]
    pub const fn percent(self) -> u16 {
        match self {
            Self::Compact => 90,
            Self::Default => 100,
            Self::Comfortable => 110,
            Self::Large => 125,
        }
    }

    /// Resolves one of the bounded values accepted by application settings.
    #[must_use]
    pub const fn from_percent(percent: u16) -> Option<Self> {
        match percent {
            90 => Some(Self::Compact),
            100 => Some(Self::Default),
            110 => Some(Self::Comfortable),
            125 => Some(Self::Large),
            _ => None,
        }
    }
}

/// Monospace size used by Ephervex diagnostics and server-console surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TechnicalTextSize {
    Compact,
    #[default]
    Default,
    Comfortable,
    Large,
}

impl TechnicalTextSize {
    /// The logical pixel size used by selectable technical text controls.
    #[must_use]
    pub const fn pixels(self) -> u16 {
        match self {
            Self::Compact => 11,
            Self::Default => 12,
            Self::Comfortable => 14,
            Self::Large => 16,
        }
    }

    /// Resolves one of the bounded values accepted by application settings.
    #[must_use]
    pub const fn from_pixels(pixels: u16) -> Option<Self> {
        match pixels {
            11 => Some(Self::Compact),
            12 => Some(Self::Default),
            14 => Some(Self::Comfortable),
            16 => Some(Self::Large),
            _ => None,
        }
    }
}

/// Which of Ephervex's own color schemes the interface paints in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApplicationColorScheme {
    #[default]
    Dark,
    Light,
}

impl ApplicationColorScheme {
    /// Whether this scheme is the dark one.
    #[must_use]
    pub const fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }

    /// A stable identifier for `--json` output and frontend state.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    /// Resolves one of the bounded values accepted by application settings.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }
}

/// The theme the interface paints in, as the preference records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplicationTheme {
    BuiltIn(ApplicationColorScheme),
    /// A theme file in the user's themes folder, by its file stem.
    User(String),
}

impl Default for ApplicationTheme {
    fn default() -> Self {
        Self::BuiltIn(ApplicationColorScheme::default())
    }
}

impl ApplicationTheme {
    /// Marks a recorded name as a user theme, so no file can shadow a
    /// built-in scheme or one a later build adds.
    const USER_PREFIX: &'static str = "user:";

    /// Longest file stem a user theme may have.
    pub const MAX_USER_ID_LENGTH: usize = 64;

    /// A user theme, if `id` is a usable file stem: lowercase ASCII letters,
    /// digits, `-` and `_`, starting with a letter or digit.
    #[must_use]
    pub fn user(id: &str) -> Option<Self> {
        let usable = id.len() <= Self::MAX_USER_ID_LENGTH
            && id
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'));
        usable.then(|| Self::User(id.to_owned()))
    }

    /// Resolves a recorded name, or `None` for one this build does not know.
    #[must_use]
    pub fn from_stored(name: &str) -> Option<Self> {
        match name.strip_prefix(Self::USER_PREFIX) {
            Some(id) => Self::user(id),
            None => ApplicationColorScheme::from_name(name).map(Self::BuiltIn),
        }
    }

    /// The name this theme is recorded under.
    #[must_use]
    pub fn to_stored(&self) -> String {
        match self {
            Self::BuiltIn(scheme) => scheme.as_str().to_owned(),
            Self::User(id) => format!("{}{id}", Self::USER_PREFIX),
        }
    }
}

/// Display preferences for Ephervex itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApplicationDisplayConfig {
    pub text_scale: ApplicationTextScale,
    pub technical_text_size: TechnicalTextSize,
    /// The chosen theme, by its recorded name. See [`ApplicationTheme`].
    ///
    /// Text rather than an enum because the set of themes grows: one a newer
    /// Ephervex wrote costs this build that one setting, not the whole file.
    /// Resolved by [`Self::theme`].
    pub color_scheme: String,
    /// Whether linked servers also appear in the global Servers list.
    pub show_linked_servers: bool,
    /// Keys this build does not know about. See [`Config::unknown`].
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl Default for ApplicationDisplayConfig {
    fn default() -> Self {
        Self {
            text_scale: ApplicationTextScale::default(),
            technical_text_size: TechnicalTextSize::default(),
            color_scheme: ApplicationColorScheme::default().as_str().to_owned(),
            show_linked_servers: false,
            unknown: toml::Table::new(),
        }
    }
}

impl ApplicationDisplayConfig {
    /// The recorded theme, or the default for a name this build does not
    /// know.
    ///
    /// A user theme is returned whether or not its file still exists; that is
    /// for whoever reads the themes folder to decide.
    #[must_use]
    pub fn theme(&self) -> ApplicationTheme {
        ApplicationTheme::from_stored(&self.color_scheme).unwrap_or_default()
    }

    /// Records a theme.
    ///
    /// A recorded name this build does not know is kept while it resolves to
    /// the same theme, so saving the other display settings leaves a newer
    /// build's choice in place.
    pub fn set_theme(&mut self, theme: &ApplicationTheme) {
        if self.theme() != *theme {
            self.color_scheme = theme.to_stored();
        }
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

/// Durable policy for browsing mod catalogs.
///
/// # What is deliberately absent
///
/// Nothing a catalog needs to be *reached*. A CurseForge API key is a
/// credential and lives in the keyring (ADR 0004), and whether one is stored is
/// a fact about the keyring rather than a setting; neither belongs in a file
/// users attach to bug reports. Naming CurseForge here says which catalog the
/// user wants to browse first, not that they can.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModsConfig {
    /// The catalog the mod browser opens on, empty for Ephervex's own default.
    ///
    /// A string rather than an enum on purpose. A newer Ephervex may know a
    /// catalog this build does not, and a typed field would fail the whole
    /// document over one value; this keeps the rest of the file readable, keeps
    /// the recorded name intact, and leaves resolving it to
    /// [`DefaultModCatalog`], which can say "not this build" instead of
    /// guessing.
    pub default_catalog: String,
    /// Whether turning a mod off, on, or deleting its jar acts on the mods
    /// that require it.
    ///
    /// Off, every switch changes exactly the mod it belongs to and nothing
    /// else. The relations stay recorded and stay on display: what this
    /// decides is whether a change follows them, not whether they are known.
    /// It exists because arranging a mod set by hand is a thing this launcher
    /// is for, and a launcher that keeps adding mods back is in the way of it.
    #[serde(default = "follow_mod_dependencies_default")]
    pub follow_mod_dependencies: bool,
    /// Whether a mod change that affects other mods stops to be confirmed.
    ///
    /// Read only when [`Self::follow_mod_dependencies`] is on: with nothing
    /// else being changed there is nothing to confirm.
    ///
    /// Off does not mean the change is made blind. The consequence is still
    /// worked out and still reported; what it stops doing is blocking. A
    /// cascade that would have been confirmed is simply applied, because
    /// somebody who turned this off has already agreed to it - leaving the
    /// dependents alone instead would hand them a mod set that does not load,
    /// which is the outcome the warning exists to prevent.
    #[serde(default = "warn_on_dependency_impact_default")]
    pub warn_on_dependency_impact: bool,
    /// Keys this build does not know about. See [`Config::unknown`].
    #[serde(flatten)]
    pub unknown: toml::Table,
}

/// Warning about a mod change that affects other mods is the default.
const fn warn_on_dependency_impact_default() -> bool {
    true
}

/// Acting on what a mod change affects is the default.
const fn follow_mod_dependencies_default() -> bool {
    true
}

/// Which hosts the user has agreed Ephervex may fetch a pack archive from.
///
/// A different trust category from the download allowlists in
/// `ephervex-download`. Those decide where an address *taken out of an
/// untrusted archive* may point, and are a fixed, audited list precisely
/// because nobody consented to them. A host here got in because a human
/// activated a link and then confirmed the address they were shown, so the
/// answer is theirs to give and theirs to take back (ADR 0027).
///
/// Remembering it only removes the second prompt. It never widens where a
/// pack's own `files[].downloads` may point, and it is not a credential store:
/// a host name is not a secret and this file is one users attach to bug
/// reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ImportConfig {
    /// Hosts a pack archive may be downloaded from without asking again, as
    /// lowercase host names with no scheme, port, or path.
    pub trusted_pack_hosts: Vec<String>,
    /// Keys this build does not know about. See [`Config::unknown`].
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl ImportConfig {
    /// Whether this host has already been agreed to.
    ///
    /// Compared after lowercasing on both sides, because a host name is
    /// case-insensitive and a user who typed one in a browser did not choose
    /// its spelling.
    #[must_use]
    pub fn trusts(&self, host: &str) -> bool {
        let host = host.trim().to_ascii_lowercase();
        !host.is_empty()
            && self
                .trusted_pack_hosts
                .iter()
                .any(|trusted| trusted.trim().to_ascii_lowercase() == host)
    }

    /// Remembers a host. Idempotent, so confirming twice records one entry.
    pub fn trust(&mut self, host: &str) {
        let host = host.trim().to_ascii_lowercase();
        if host.is_empty() || self.trusts(&host) {
            return;
        }
        self.trusted_pack_hosts.push(host);
        self.trusted_pack_hosts.sort();
    }

    /// Withdraws a host. Returns whether anything was recorded for it.
    pub fn withdraw(&mut self, host: &str) -> bool {
        let host = host.trim().to_ascii_lowercase();
        let before = self.trusted_pack_hosts.len();
        self.trusted_pack_hosts
            .retain(|trusted| trusted.trim().to_ascii_lowercase() != host);
        self.trusted_pack_hosts.len() != before
    }
}

impl ModsConfig {
    /// The recorded catalog policy.
    #[must_use]
    pub fn default_catalog(&self) -> DefaultModCatalog {
        DefaultModCatalog::from_stored(&self.default_catalog)
    }

    /// Records a catalog choice, or clears it back to Ephervex's own default.
    pub fn set_default_catalog(&mut self, catalog: Option<ModProvider>) {
        catalog
            .map(ModProvider::as_str)
            .unwrap_or_default()
            .clone_into(&mut self.default_catalog);
    }
}

impl Default for ModsConfig {
    fn default() -> Self {
        Self {
            default_catalog: String::new(),
            follow_mod_dependencies: follow_mod_dependencies_default(),
            warn_on_dependency_impact: warn_on_dependency_impact_default(),
            unknown: toml::Table::new(),
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
    /// Apply the selected account to new development servers when its identity
    /// is valid for the server's authentication mode.
    pub make_default_account_operator: bool,
    /// Permission level written to the new server's individual operator entry.
    pub operator_permission_level: OperatorPermissionLevel,
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
            make_default_account_operator: true,
            operator_permission_level: OperatorPermissionLevel::default(),
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
/// The API key is not, and its absence is the point - it is a credential, so it
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
/// usable is in the keyring and is not represented here at all - not even as a
/// copy, and not even as a length.
// No `Eq`: the open `unknown` table holds `toml::Value`, which has no total
// equality because it can carry a float.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AccountConfig {
    /// The account's Minecraft display name, for showing in the interface
    /// without a network round trip.
    pub name: String,
    /// Microsoft or offline.
    ///
    /// Absent in a file written before offline accounts existed, and
    /// [`AccountKind::default`] reads that as Microsoft - which is what such a
    /// row is.
    pub kind: AccountKind,
    /// RFC 3339 timestamp of when this account was first added.
    pub added_at: String,
    /// RFC 3339 timestamp of the last successful chain completion, or of
    /// creation for an offline account, which never authenticates.
    pub last_authenticated_at: String,
    /// Keys this build does not know about. See [`Config::unknown`].
    ///
    /// An account row is as open as any other table: a newer Ephervex may
    /// record something here, and an older one running once must not destroy
    /// it. That openness is also why this table is walked for credential-shaped
    /// keys like every other one -- nothing about being nested under a profile
    /// id makes an untrusted key safe.
    #[serde(flatten)]
    pub unknown: toml::Table,
}

/// A client's paired linked server, keyed in [`Config::linked_pairs`] by the
/// client's id. Without an entry, a client is paired with its first linked
/// server.
///
/// Workflow state rather than the link itself: the server row owns
/// `linked_client_id`, and a pair only chooses one of the servers already
/// linked to its client (ADR 0040).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LinkedPairConfig {
    /// The paired server's id.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub server: String,
    /// Whether starting either side also starts the other.
    pub start_together: bool,
    /// The first linked server the user unpaired, which is not paired again
    /// by default while it stays the client's first linked server.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub unpaired_from: String,
    /// Keys this build does not know about. See [`Config::unknown`].
    #[serde(flatten)]
    pub unknown: toml::Table,
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

/// Whether the shared reusable artifact cache has a byte budget at all.
///
/// A mode beside the byte count rather than a sentinel inside it. `0` and
/// `u64::MAX` are both arithmetically valid budgets, so encoding "no limit" as
/// either would mean every reader has to remember which number is not a number.
/// Keeping them separate also lets the file remember the byte value a user had
/// configured while they try Unlimited, so switching back restores it instead of
/// silently resetting to the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactCacheLimitMode {
    #[default]
    Limited,
    Unlimited,
}

/// How much disk the disposable caches may use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    pub artifact_limit_mode: ArtifactCacheLimitMode,
    /// The budget in bytes, always. Not mebibytes, not gibibytes: a unit is a
    /// presentation choice, and storing one would make the file's meaning depend
    /// on which screen last wrote it.
    pub artifact_limit_bytes: u64,
    /// Keys this build does not know about. See [`Config::unknown`].
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            artifact_limit_mode: ArtifactCacheLimitMode::Limited,
            artifact_limit_bytes: DEFAULT_ARTIFACT_CACHE_LIMIT_BYTES,
            unknown: toml::Table::new(),
        }
    }
}

impl CacheConfig {
    /// The typed budget this table describes.
    ///
    /// A byte count the type refuses - a hand-edited zero, say - degrades to the
    /// default rather than failing the whole configuration load, for the same
    /// reason [`Config::active_account`] does: one implausible value in a file
    /// full of deliberate settings should cost the user that value, not all of
    /// them.
    #[must_use]
    pub fn artifact_limit(&self) -> ArtifactCacheLimit {
        match self.artifact_limit_mode {
            ArtifactCacheLimitMode::Unlimited => ArtifactCacheLimit::Unlimited,
            ArtifactCacheLimitMode::Limited => {
                ArtifactCacheLimit::from_bytes(self.artifact_limit_bytes).unwrap_or_else(|error| {
                    tracing::warn!(
                        bytes = self.artifact_limit_bytes,
                        %error,
                        "the configured artifact cache limit is not usable; using the default"
                    );
                    ArtifactCacheLimit::default()
                })
            }
        }
    }

    /// Records a budget.
    ///
    /// Switching to Unlimited keeps the byte value, so turning the limit back on
    /// returns the user to the number they chose.
    pub const fn set_artifact_limit(&mut self, limit: ArtifactCacheLimit) {
        match limit {
            ArtifactCacheLimit::Limited(bytes) => {
                self.artifact_limit_mode = ArtifactCacheLimitMode::Limited;
                self.artifact_limit_bytes = bytes;
            }
            ArtifactCacheLimit::Unlimited => {
                self.artifact_limit_mode = ArtifactCacheLimitMode::Unlimited;
            }
        }
    }
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
    /// Folders a download watcher checks for a manually placed copy of a
    /// file its provider refused to serve automatically, matched by hash.
    ///
    /// `None` means "use the OS Downloads folder", this launcher's own
    /// default; `Some(vec![])` means the user explicitly cleared the list
    /// rather than never having set it. Unlike
    /// [`Self::persistent_instance_root`]'s single relocatable root, a list
    /// cannot use blank-vs-populated to tell those two apart.
    pub watched_download_folders: Option<Vec<String>>,
    /// The account launches use, as a profile UUID, or empty when nobody is
    /// signed in.
    ///
    /// Separate from [`Self::accounts`] so that "which one is active" is one
    /// value rather than a flag that two rows could both claim. One account
    /// ships, but the shape does not foreclose a second.
    pub active_account: String,
    pub display: ApplicationDisplayConfig,
    pub java: JavaConfig,
    /// Minecraft releases the user works with often, in the order they should
    /// be offered.
    ///
    /// Stored verbatim and never validated against a catalog: a favorite is a
    /// statement about the user's own work, and a release that is momentarily
    /// unreachable is not one they have stopped caring about. An entry the
    /// current catalog has no version for is simply not shown.
    ///
    /// A plain list rather than a set, because the order is the setting: it is
    /// what decides which release the New Instance dialog offers first.
    pub favorite_minecraft_versions: Vec<String>,
    /// Managed Minecraft preferences, keyed by preference name. An open map
    /// rather than a fixed struct because the set of managed keys grows with
    /// the version-aware options adapters.
    pub minecraft_preferences: BTreeMap<String, PreferenceSetting>,
    /// Policy for browsing mod catalogs. Which catalog, never how to reach one.
    pub mods: ModsConfig,
    /// Which hosts the user has agreed a pack may be downloaded from.
    pub import: ImportConfig,
    pub server_defaults: ServerDefaults,
    pub eula: EulaConfig,
    pub api: ApiConfig,
    /// Budgets for the disposable caches. Never cache *contents* - those are
    /// files under the cache root, and configuration is not where a launcher
    /// records what it happens to have downloaded.
    pub cache: CacheConfig,
    /// Non-secret metadata for accounts that have signed in, keyed by profile
    /// UUID.
    ///
    /// A map rather than a single `[account]` table because Ephervex supports
    /// one account ships, and the data model must not make a second one a
    /// schema change. Same arrangement as [`Self::minecraft_preferences`].
    pub accounts: BTreeMap<String, AccountConfig>,
    /// Accounts removed while the keyring could not be reached, whose stored
    /// credential is still to be deleted, so it is never left with nothing
    /// naming it (ADR 0004). Account ids only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keyring_cleanup: Vec<String>,
    /// Each client's chosen pair, keyed by the client's id. An entry whose
    /// client is gone is ignored, and one whose server is gone or no longer
    /// linked falls back to the client's first linked server.
    pub linked_pairs: BTreeMap<String, LinkedPairConfig>,
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
            watched_download_folders: None,
            active_account: String::new(),
            display: ApplicationDisplayConfig::default(),
            java: JavaConfig::default(),
            favorite_minecraft_versions: Vec::new(),
            minecraft_preferences: BTreeMap::new(),
            mods: ModsConfig::default(),
            import: ImportConfig::default(),
            server_defaults: ServerDefaults::default(),
            eula: EulaConfig::default(),
            api: ApiConfig::default(),
            cache: CacheConfig::default(),
            accounts: BTreeMap::new(),
            keyring_cleanup: Vec::new(),
            linked_pairs: BTreeMap::new(),
            unknown: toml::Table::new(),
        }
    }
}

impl Config {
    /// Loads the configuration, falling back to defaults when the file does not
    /// exist yet.
    ///
    /// A missing file is a first run, not an error, and nothing is written -
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
                source: sanitized_parse_error(text, &source),
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
                    source: sanitized_parse_error(text, &source),
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

    /// Applies `change` to the configuration on disk and writes the result.
    ///
    /// The file is reloaded under an exclusive lock first, so a change another
    /// process or `App` wrote in the meantime is kept rather than reverted.
    /// Returns what was written.
    pub fn update(paths: &Paths, change: impl FnOnce(&mut Self)) -> Result<Self, ConfigError> {
        crate::paths::create_dir_all(paths.config_dir())?;
        let _writing = ConfigWriteLock::take(&paths.config_dir().join(CONFIG_LOCK_FILE))?;
        let mut config = Self::load_or_default(paths)?;
        change(&mut config);
        config.save_to(&paths.config_file())?;
        Ok(config)
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
        crate::atomic::write_bytes_atomic(
            path,
            text.as_bytes(),
            crate::atomic::AtomicWriteOptions::private(),
        )
        .map_err(|source| ConfigError::Unwritable {
            path: path.to_path_buf(),
            source,
        })
    }

    /// The configured persistent instance root, or `None` for the XDG default.
    #[must_use]
    pub fn persistent_instance_root(&self) -> Option<PathBuf> {
        let trimmed = self.persistent_instance_root.trim();
        (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
    }

    /// The folders a download watcher checks, resolving the unconfigured
    /// default to the OS Downloads folder.
    ///
    /// Empty when the user explicitly cleared the list, or when this build
    /// has no configured list and the platform reports no Downloads folder
    /// either (there is nothing sensible to fall back to further).
    #[must_use]
    pub fn watched_download_folders(&self) -> Vec<PathBuf> {
        match &self.watched_download_folders {
            Some(folders) => folders
                .iter()
                .map(|folder| folder.trim())
                .filter(|folder| !folder.is_empty())
                .map(PathBuf::from)
                .collect(),
            None => directories::UserDirs::new()
                .and_then(|dirs| dirs.download_dir().map(Path::to_path_buf))
                .into_iter()
                .collect(),
        }
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
        // A remembered account's credential is current, not one to clean up.
        self.keyring_cleanup.retain(|pending| pending != id);
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
    /// before this one, which is the least surprising thing to fall back to -
    /// and it beats leaving no default at all, which would make the next launch
    /// fail for a user who still has a perfectly good account.
    ///
    /// The stored credential is removed separately, by whoever owns the keyring
    /// - this crate has no business touching it (ADR 0004).
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

    /// Records an account whose stored credential could not be deleted yet,
    /// for deletion once the keyring can be reached.
    pub fn record_keyring_cleanup(&mut self, id: &str) {
        if !self.keyring_cleanup.iter().any(|pending| pending == id) {
            self.keyring_cleanup.push(id.to_owned());
        }
    }

    /// Finds an account by UUID or by display name.
    ///
    /// Names are matched case-insensitively because the user typed them, and
    /// an id is tried first because it is unambiguous by construction. A name
    /// held by more than one account resolves to nothing rather than to an
    /// arbitrary one - removing the wrong account is not recoverable.
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
            .map(|found| found.reported)
            .or_else(|| find_secret_key(&self.java.unknown, "java").map(|found| found.reported))
            .or_else(|| {
                find_secret_key(&self.display.unknown, "display").map(|found| found.reported)
            })
            .or_else(|| {
                find_secret_key(&self.server_defaults.unknown, "server_defaults")
                    .map(|found| found.reported)
            })
            .or_else(|| find_secret_key(&self.eula.unknown, "eula").map(|found| found.reported))
            // `[api]` is the table a hand edit is most likely to put a
            // CurseForge key into, since that is where the settings screen
            // shows the field.
            .or_else(|| find_secret_key(&self.api.unknown, "api").map(|found| found.reported))
            .or_else(|| find_secret_key(&self.cache.unknown, "cache").map(|found| found.reported))
            .or_else(|| find_secret_key(&self.mods.unknown, "mods").map(|found| found.reported))
            .or_else(|| {
                self.minecraft_preferences
                    .keys()
                    .find(|key| looks_like_secret_key(key))
                    .map(|_| "minecraft_preferences.<redacted>".to_owned())
            })
            .or_else(|| {
                self.accounts
                    .keys()
                    .find(|key| looks_like_secret_key(key))
                    .map(|_| "accounts.<redacted>".to_owned())
            })
            .or_else(|| {
                // The profile id a row is keyed by is not secret, but it is
                // still user-controlled text that should not reach a report.
                self.accounts
                    .values()
                    .find_map(|account| find_secret_key(&account.unknown, "accounts"))
                    .map(|_| "accounts.<redacted>".to_owned())
            })
            .or_else(|| {
                self.linked_pairs
                    .keys()
                    .find(|key| looks_like_secret_key(key))
                    .map(|_| "linked_pairs.<redacted>".to_owned())
            })
            .or_else(|| {
                self.linked_pairs
                    .values()
                    .find_map(|pair| find_secret_key(&pair.unknown, "linked_pairs"))
                    .map(|_| "linked_pairs.<redacted>".to_owned())
            })
    }

    fn strip_secret_looking_keys(&mut self) {
        for (table, prefix) in [
            (&mut self.unknown, ""),
            (&mut self.java.unknown, "java"),
            (&mut self.display.unknown, "display"),
            (&mut self.server_defaults.unknown, "server_defaults"),
            (&mut self.eula.unknown, "eula"),
            (&mut self.api.unknown, "api"),
            (&mut self.cache.unknown, "cache"),
            (&mut self.mods.unknown, "mods"),
        ] {
            while let Some(found) = find_secret_key(table, prefix) {
                // The key can itself contain credential material, so neither
                // the value nor the untrusted key is safe to log.
                tracing::warn!(
                    "discarding a credential-looking key from the configuration file; \
                     secrets belong in the system keyring"
                );
                remove_nested(table, &found.leaf);
            }
        }

        for account in self.accounts.values_mut() {
            while let Some(found) = find_secret_key(&account.unknown, "accounts") {
                tracing::warn!(
                    "discarding a credential-looking key from an account row; \
                     secrets belong in the system keyring"
                );
                remove_nested(&mut account.unknown, &found.leaf);
            }
        }

        for pair in self.linked_pairs.values_mut() {
            while let Some(found) = find_secret_key(&pair.unknown, "linked_pairs") {
                tracing::warn!(
                    "discarding a credential-looking key from a linked pair; \
                     secrets belong in the system keyring"
                );
                remove_nested(&mut pair.unknown, &found.leaf);
            }
        }

        let preference_count = self.minecraft_preferences.len();
        self.minecraft_preferences
            .retain(|key, _| !looks_like_secret_key(key));
        let account_count = self.accounts.len();
        self.accounts.retain(|key, _| !looks_like_secret_key(key));
        let pair_count = self.linked_pairs.len();
        self.linked_pairs
            .retain(|key, _| !looks_like_secret_key(key));
        if self.minecraft_preferences.len() != preference_count
            || self.accounts.len() != account_count
            || self.linked_pairs.len() != pair_count
        {
            tracing::warn!(
                "discarding a credential-looking key from the configuration file; \
                 secrets belong in the system keyring"
            );
        }
    }
}

fn sanitized_parse_error(text: &str, error: &toml::de::Error) -> ConfigParseError {
    let Some(offset) = error
        .span()
        .map(|span| span.start)
        .filter(|offset| *offset <= text.len())
    else {
        return ConfigParseError {
            line: None,
            column: None,
        };
    };
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = before
        .rsplit_once('\n')
        .map_or(before, |(_, current)| current)
        .chars()
        .count()
        + 1;
    ConfigParseError {
        line: Some(line),
        column: Some(column),
    }
}

/// Finds the first key at any depth whose name matches [`SECRET_KEY_MARKERS`].
struct SecretKeyMatch {
    reported: String,
    leaf: String,
}

fn find_secret_key(table: &toml::Table, prefix: &str) -> Option<SecretKeyMatch> {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if looks_like_secret_key(key) {
            let reported = if prefix.is_empty() {
                "<redacted>".to_owned()
            } else {
                format!("{prefix}.<redacted>")
            };
            return Some(SecretKeyMatch {
                reported,
                leaf: key.clone(),
            });
        }
        match value {
            toml::Value::Table(nested) => {
                if let Some(found) = find_secret_key(nested, &path) {
                    return Some(found);
                }
            }
            // An array of tables hides a key exactly as well as a table does,
            // and TOML's `[[section]]` syntax makes one an obvious thing to
            // hand-edit a credential into.
            toml::Value::Array(elements) => {
                if let Some(found) = find_secret_in_array(elements, &path) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

/// Walks the tables inside an array, and inside any array they contain.
///
/// The reported path stays the array's own, without an element index: it is
/// shown to a user to tell them which part of their configuration to edit, and
/// an index into a list they are looking at adds nothing.
fn find_secret_in_array(elements: &[toml::Value], prefix: &str) -> Option<SecretKeyMatch> {
    for element in elements {
        let found = match element {
            toml::Value::Table(nested) => find_secret_key(nested, prefix),
            toml::Value::Array(nested) => find_secret_in_array(nested, prefix),
            _ => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

fn looks_like_secret_key(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    SECRET_KEY_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

/// Removes one occurrence of a key by leaf name, at any depth.
///
/// Callers loop until [`find_secret_key`] reports nothing, so removing a single
/// occurrence per call is enough to clear all of them.
fn remove_nested(table: &mut toml::Table, leaf: &str) {
    if table.remove(leaf).is_some() {
        return;
    }
    for (_, value) in table.iter_mut() {
        remove_nested_value(value, leaf);
    }
}

/// The array half of [`remove_nested`], so a key found inside an array of
/// tables can also be removed rather than only reported.
fn remove_nested_value(value: &mut toml::Value, leaf: &str) {
    match value {
        toml::Value::Table(nested) => remove_nested(nested, leaf),
        toml::Value::Array(elements) => {
            for element in elements {
                remove_nested_value(element, leaf);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::AccountKind;

    use super::*;

    #[test]
    fn signing_an_account_in_again_cancels_its_pending_keyring_cleanup() {
        let mut config = Config::default();
        config.remember_account(
            "id",
            "Steve",
            AccountKind::Microsoft,
            "2026-01-01T00:00:00Z",
        );
        config.forget_account("id");
        config.record_keyring_cleanup("id");
        assert_eq!(config.keyring_cleanup, ["id"]);
        assert!(!config.accounts.contains_key("id"));

        config.remember_account(
            "id",
            "Steve",
            AccountKind::Microsoft,
            "2026-01-02T00:00:00Z",
        );
        assert!(config.keyring_cleanup.is_empty());
    }

    #[test]
    fn pending_keyring_cleanup_round_trips_and_is_not_taken_for_a_secret() {
        let mut config = Config::default();
        config.keyring_cleanup.push("id".to_owned());
        let text = config.to_toml_string().expect("serialize");
        assert_eq!(
            Config::from_toml_str(&text).expect("parse").keyring_cleanup,
            ["id"]
        );
        assert!(
            !Config::default()
                .to_toml_string()
                .expect("serialize")
                .contains("keyring_cleanup")
        );
    }

    #[test]
    fn concurrent_updates_keep_every_change() {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path());
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for writer in ["a", "b"] {
                let (paths, barrier) = (&paths, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    for round in 0..20 {
                        Config::update(paths, |config| {
                            config
                                .favorite_minecraft_versions
                                .push(format!("{writer}{round}"));
                        })
                        .expect("update");
                    }
                });
            }
        });

        let written = Config::load_or_default(&paths).expect("load");
        assert_eq!(written.favorite_minecraft_versions.len(), 40);
    }

    #[test]
    fn an_update_waits_for_another_process_writing_the_configuration() {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path());
        crate::paths::create_dir_all(paths.config_dir()).expect("config dir");
        // A descriptor of its own, as another process would have.
        let other_writer = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(paths.config_dir().join(CONFIG_LOCK_FILE))
            .expect("lock file");
        rustix::fs::flock(&other_writer, rustix::fs::FlockOperation::LockExclusive).expect("flock");

        let waiting = std::thread::spawn({
            let paths = paths.clone();
            move || Config::update(&paths, |config| config.display.show_linked_servers = true)
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !waiting.is_finished(),
            "nothing is written while another update runs"
        );

        drop(other_writer);
        assert!(
            waiting
                .join()
                .expect("no panic")
                .expect("update")
                .display
                .show_linked_servers
        );
    }

    #[test]
    fn an_update_keeps_what_another_writer_saved_after_this_one_loaded() {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path());
        let stale = Config::load_or_default(&paths).expect("load");

        let mut other = stale.clone();
        other.remember_account("id", "Alice", AccountKind::Offline, "2026-01-01T00:00:00Z");
        other.save(&paths).expect("save");

        let updated = Config::update(&paths, |config| config.display.show_linked_servers = true)
            .expect("update");
        assert!(updated.accounts.contains_key("id"));
        assert!(updated.display.show_linked_servers);
        assert_eq!(Config::load_or_default(&paths).expect("load"), updated);
    }

    /// The serialized configuration schema.
    const DEFAULT_CONFIG_EXAMPLE: &str = r#"
schema_version = 1
language = "en-US"
persistent_instance_root = ""

[display]
text_scale = "default"
technical_text_size = "default"
show_linked_servers = false

[java]
auto_detect = true
auto_download = true
min_memory_mib = 512
max_memory_mib = 4096
jvm_args = []

[server_defaults]
online_mode = false
network_access = "localhost"
port = 25565
make_default_account_operator = true
operator_permission_level = 4

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
    fn a_configuration_file_written_before_cache_budgets_gets_the_ten_gibibyte_default() {
        // The example above deliberately has no `[cache]` table, which is
        // exactly what a file written by an earlier build looks like.
        assert!(!DEFAULT_CONFIG_EXAMPLE.contains("[cache]"));
        let parsed = Config::from_toml_str(DEFAULT_CONFIG_EXAMPLE).expect("parse");
        assert_eq!(
            parsed.cache.artifact_limit(),
            ArtifactCacheLimit::Limited(10 * 1024 * 1024 * 1024)
        );
        assert_eq!(
            parsed.cache.artifact_limit_mode,
            ArtifactCacheLimitMode::Limited
        );
    }

    #[test]
    fn a_large_custom_cache_budget_round_trips_through_the_settings_file() {
        for bytes in [
            500 * 1024 * 1024 * 1024_u64,
            1024 * 1024 * 1024 * 1024,
            4 * 1024 * 1024 * 1024 * 1024,
            ephervex_model::MAX_ARTIFACT_CACHE_LIMIT_BYTES,
        ] {
            let mut config = Config::default();
            config
                .cache
                .set_artifact_limit(ArtifactCacheLimit::Limited(bytes));

            let text = config.to_toml_string().expect("serialize");
            let parsed = Config::from_toml_str(&text).expect("parse");
            assert_eq!(
                parsed.cache.artifact_limit(),
                ArtifactCacheLimit::Limited(bytes),
                "{bytes} bytes must survive a save and a load"
            );
        }
    }

    #[test]
    fn an_unlimited_cache_round_trips_and_remembers_the_previous_byte_value() {
        let mut config = Config::default();
        config
            .cache
            .set_artifact_limit(ArtifactCacheLimit::Limited(500 * 1024 * 1024 * 1024));
        config
            .cache
            .set_artifact_limit(ArtifactCacheLimit::Unlimited);

        let text = config.to_toml_string().expect("serialize");
        let mut parsed = Config::from_toml_str(&text).expect("parse");
        assert_eq!(parsed.cache.artifact_limit(), ArtifactCacheLimit::Unlimited);

        // Turning the budget back on returns the number the user chose rather
        // than resetting them to the default.
        parsed.cache.artifact_limit_mode = ArtifactCacheLimitMode::Limited;
        assert_eq!(
            parsed.cache.artifact_limit(),
            ArtifactCacheLimit::Limited(500 * 1024 * 1024 * 1024)
        );
    }

    #[test]
    fn an_implausible_hand_edited_cache_budget_degrades_to_the_default() {
        for bytes in [0, 1, 1023] {
            let text = format!("[cache]\nartifact_limit_bytes = {bytes}\n");
            let parsed = Config::from_toml_str(&text).expect("the rest of the file still loads");
            assert_eq!(
                parsed.cache.artifact_limit(),
                ArtifactCacheLimit::default(),
                "{bytes} is not a usable budget"
            );
        }
    }

    #[test]
    fn defaults_match_the_product_decisions_they_encode() {
        let config = Config::default();
        assert_eq!(config.schema_version, CONFIG_SCHEMA_VERSION);
        assert_eq!(config.language, "en-US");
        assert_eq!(config.display.text_scale.percent(), 100);
        assert_eq!(config.display.technical_text_size.pixels(), 12);
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
    fn configuration_without_display_settings_loads_current_application_defaults() {
        let legacy = DEFAULT_CONFIG_EXAMPLE
            .lines()
            .filter(|line| {
                !matches!(
                    *line,
                    "[display]"
                        | "text_scale = \"default\""
                        | "technical_text_size = \"default\""
                        | "show_linked_servers = false"
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let parsed = Config::from_toml_str(&legacy).expect("legacy configuration parses");
        assert_eq!(parsed.display, ApplicationDisplayConfig::default());
        assert!(!parsed.display.show_linked_servers);
        assert!(parsed.server_defaults.make_default_account_operator);
        assert_eq!(parsed.server_defaults.operator_permission_level.value(), 4);
    }

    #[test]
    fn application_display_choices_round_trip_without_affecting_minecraft_preferences() {
        let mut config = Config::default();
        config.display.text_scale = ApplicationTextScale::Large;
        config.display.technical_text_size = TechnicalTextSize::Comfortable;
        config.display.show_linked_servers = true;
        config.server_defaults.make_default_account_operator = false;
        config.server_defaults.operator_permission_level =
            OperatorPermissionLevel::from_value(2).expect("valid level");
        let text = config.to_toml_string().expect("serialize");
        let parsed = Config::from_toml_str(&text).expect("re-parse");
        assert_eq!(parsed.display, config.display);
        assert_eq!(parsed.server_defaults, config.server_defaults);
        assert!(parsed.minecraft_preferences.is_empty());
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

        // No unique temporary file used for the atomic rename may survive.
        assert!(
            std::fs::read_dir(paths.config_dir())
                .expect("config directory")
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp."))
        );
    }

    #[cfg(unix)]
    #[test]
    fn configuration_publication_ignores_predictable_symlinks_and_creates_a_private_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("config.toml");
        let sentinel = directory.path().join("sentinel");
        std::fs::write(&sentinel, "keep").expect("sentinel");
        std::os::unix::fs::symlink(&sentinel, path.with_extension("toml.tmp"))
            .expect("plant old staging symlink");

        Config::default().save_to(&path).expect("save");

        assert_eq!(
            std::fs::read_to_string(&sentinel).expect("sentinel"),
            "keep"
        );
        let mode = std::fs::metadata(&path)
            .expect("config metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_configuration_without_a_mods_table_browses_the_built_in_catalog() {
        // Every configuration written before the preference existed looks like
        // this, and must keep behaving the way the launcher already did.
        let config = Config::from_toml_str(DEFAULT_CONFIG_EXAMPLE).expect("parse");
        assert_eq!(config.mods.default_catalog(), DefaultModCatalog::BuiltIn);
        assert_eq!(
            config.mods.default_catalog().resolved(),
            Some(ModProvider::Modrinth)
        );
    }

    #[test]
    fn a_recorded_catalog_round_trips_through_the_file() {
        let mut config = Config::default();
        config
            .mods
            .set_default_catalog(Some(ModProvider::CurseForge));
        let text = config.to_toml_string().expect("serialize");
        assert!(text.contains("default_catalog = \"curseforge\""));

        let reloaded = Config::from_toml_str(&text).expect("re-parse");
        assert_eq!(
            reloaded.mods.default_catalog(),
            DefaultModCatalog::Selected(ModProvider::CurseForge)
        );
    }

    #[test]
    fn recording_the_built_in_catalog_is_kept_apart_from_recording_nothing() {
        let mut config = Config::default();
        config.mods.set_default_catalog(Some(ModProvider::Modrinth));
        let stored =
            Config::from_toml_str(&config.to_toml_string().expect("serialize")).expect("re-parse");
        assert_eq!(
            stored.mods.default_catalog(),
            DefaultModCatalog::Selected(ModProvider::Modrinth)
        );

        config.mods.set_default_catalog(None);
        let cleared =
            Config::from_toml_str(&config.to_toml_string().expect("serialize")).expect("re-parse");
        assert_eq!(cleared.mods.default_catalog(), DefaultModCatalog::BuiltIn);
    }

    #[test]
    fn a_catalog_a_newer_ephervex_recorded_is_kept_rather_than_reinterpreted() {
        let text =
            format!("{DEFAULT_CONFIG_EXAMPLE}\n[mods]\ndefault_catalog = \"a-later-catalog\"\n");
        let config = Config::from_toml_str(&text).expect("parse");
        assert_eq!(
            config.mods.default_catalog(),
            DefaultModCatalog::Unrecognized("a-later-catalog".to_owned()),
        );
        // The rest of the file is still readable: one value this build cannot
        // resolve costs the user that value, not the whole configuration.
        assert_eq!(config.language, "en-US");

        let rewritten = config.to_toml_string().expect("serialize");
        assert!(rewritten.contains("a-later-catalog"));
        assert_eq!(Config::from_toml_str(&rewritten).expect("re-parse"), config);
    }

    #[test]
    fn a_malformed_catalog_value_fails_the_document_rather_than_inventing_one() {
        // A type the field cannot hold is a broken file, not a preference: the
        // load fails and nothing is silently substituted.
        let text = format!("{DEFAULT_CONFIG_EXAMPLE}\n[mods]\ndefault_catalog = 7\n");
        assert!(matches!(
            Config::from_toml_str(&text),
            Err(ConfigError::Invalid { .. })
        ));
    }

    #[test]
    fn unknown_keys_inside_the_mods_table_survive() {
        let text = format!(
            "{DEFAULT_CONFIG_EXAMPLE}\n[mods]\ndefault_catalog = \"\"\nfuture_mod_option = \"q\"\n"
        );
        let config = Config::from_toml_str(&text).expect("parse");
        assert_eq!(
            config
                .mods
                .unknown
                .get("future_mod_option")
                .and_then(toml::Value::as_str),
            Some("q")
        );
        assert!(
            config
                .to_toml_string()
                .expect("serialize")
                .contains("future_mod_option")
        );
    }

    #[test]
    fn the_recorded_catalog_carries_no_credential_material() {
        let mut config = Config::default();
        config
            .mods
            .set_default_catalog(Some(ModProvider::CurseForge));
        let text = config.to_toml_string().expect("serialize");
        assert!(config.find_secret_looking_key().is_none());
        for marker in SECRET_KEY_MARKERS {
            assert!(
                !text.to_ascii_lowercase().contains(marker),
                "the configuration file must not gain a credential-looking key"
            );
        }
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

    fn with_color_scheme(line: &str) -> String {
        DEFAULT_CONFIG_EXAMPLE.replace(
            "show_linked_servers = false",
            &format!("show_linked_servers = false\n{line}"),
        )
    }

    fn user_theme(id: &str) -> ApplicationTheme {
        ApplicationTheme::user(id).expect("a usable theme id")
    }

    #[test]
    fn recorded_themes_load_as_the_theme_they_name() {
        let missing = Config::from_toml_str(DEFAULT_CONFIG_EXAMPLE).expect("parse");
        assert_eq!(
            missing.display.theme(),
            ApplicationTheme::BuiltIn(ApplicationColorScheme::Dark)
        );
        for (line, theme) in [
            (
                "color_scheme = \"dark\"",
                ApplicationTheme::BuiltIn(ApplicationColorScheme::Dark),
            ),
            (
                "color_scheme = \"light\"",
                ApplicationTheme::BuiltIn(ApplicationColorScheme::Light),
            ),
            ("color_scheme = \"user:modrinth\"", user_theme("modrinth")),
        ] {
            let config = Config::from_toml_str(&with_color_scheme(line)).expect("parse");
            assert_eq!(config.display.theme(), theme, "{line}");
        }
    }

    /// Older builds read the built-in schemes as an enum, so a file this build
    /// writes for one has to keep naming it the way they read it.
    #[test]
    fn every_theme_is_written_by_a_name_that_round_trips() {
        let default = Config::default().to_toml_string().expect("serialize");
        assert!(default.contains("color_scheme = \"dark\""));

        for (theme, written) in [
            (
                ApplicationTheme::BuiltIn(ApplicationColorScheme::Light),
                "color_scheme = \"light\"",
            ),
            (user_theme("modrinth"), "color_scheme = \"user:modrinth\""),
        ] {
            let mut config = Config::default();
            config.display.set_theme(&theme);
            let text = config.to_toml_string().expect("serialize");
            assert!(text.contains(written), "{text}");
            let parsed = Config::from_toml_str(&text).expect("re-parse");
            assert_eq!(parsed.display.theme(), theme);
            assert_eq!(parsed.display, config.display);
        }
    }

    /// A theme a newer build added costs this build the one setting. It paints
    /// in the default, and the recorded name survives until the user picks a
    /// different theme.
    #[test]
    fn a_theme_this_build_does_not_know_is_kept_rather_than_refused() {
        let text = with_color_scheme("color_scheme = \"high_contrast\"");
        let mut config = Config::from_toml_str(&text).expect("the rest of the file still loads");
        assert_eq!(config.display.theme(), ApplicationTheme::default());

        config.display.set_theme(&ApplicationTheme::default());
        let rewritten = config.to_toml_string().expect("serialize");
        assert!(rewritten.contains("color_scheme = \"high_contrast\""));

        config
            .display
            .set_theme(&ApplicationTheme::BuiltIn(ApplicationColorScheme::Light));
        assert_eq!(config.display.color_scheme, "light");
    }

    #[test]
    fn a_user_theme_id_is_a_plain_file_stem() {
        for usable in ["modrinth", "my-theme_2", "9lives"] {
            assert!(ApplicationTheme::user(usable).is_some(), "{usable}");
        }
        let too_long = "a".repeat(ApplicationTheme::MAX_USER_ID_LENGTH + 1);
        for unusable in [
            "",
            "-dash",
            "Upper",
            "dot.ted",
            "../escape",
            "sp ace",
            &too_long,
        ] {
            assert!(ApplicationTheme::user(unusable).is_none(), "{unusable}");
            assert!(
                ApplicationTheme::from_stored(&format!("user:{unusable}")).is_none(),
                "{unusable}"
            );
        }
        // Without the prefix a name is a built-in scheme or nothing.
        assert_eq!(ApplicationTheme::from_stored("modrinth"), None);
    }

    #[test]
    fn unknown_keys_inside_known_tables_also_survive() {
        let text = DEFAULT_CONFIG_EXAMPLE
            .replace("jvm_args = []", "jvm_args = []\nfuture_java_option = \"x\"")
            .replace(
                "show_linked_servers = false",
                "show_linked_servers = false\nfuture_display_option = \"y\"",
            )
            .replace(
                "operator_permission_level = 4",
                "operator_permission_level = 4\nfuture_server_option = \"z\"",
            );
        let config = Config::from_toml_str(&text).expect("parse");
        assert_eq!(
            config
                .java
                .unknown
                .get("future_java_option")
                .and_then(toml::Value::as_str),
            Some("x")
        );
        assert_eq!(
            config
                .display
                .unknown
                .get("future_display_option")
                .and_then(toml::Value::as_str),
            Some("y")
        );
        assert_eq!(
            config
                .server_defaults
                .unknown
                .get("future_server_option")
                .and_then(toml::Value::as_str),
            Some("z")
        );
        let rewritten = config.to_toml_string().expect("serialize");
        for key in [
            "future_java_option",
            "future_display_option",
            "future_server_option",
        ] {
            assert!(rewritten.contains(key));
        }
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
    fn malformed_configuration_diagnostics_do_not_echo_credential_material() {
        const CREDENTIAL: &str = "UNIQUE-CREDENTIAL-MATERIAL";
        let text = format!("refresh_token = \"{CREDENTIAL}\" trailing");
        let error = Config::from_toml_str(&text).expect_err("invalid TOML");
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains(CREDENTIAL));
        assert!(!rendered.contains("refresh_token"));
        let ConfigError::Invalid { source, .. } = error else {
            panic!("unexpected error");
        };
        assert!(source.to_string().contains("line 1"));
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
    fn an_unconfigured_watch_list_and_an_explicitly_cleared_one_round_trip_distinctly() {
        // Unlike a single relocatable root, a list cannot tell "never set"
        // apart from "cleared" by blank-vs-populated, so this exercises the
        // actual TOML round trip rather than trusting that `None` and
        // `Some(vec![])` serialize to different things.
        let mut config = Config::default();
        assert_eq!(config.watched_download_folders, None);
        let unconfigured = config.to_toml_string().expect("serialize");
        assert_eq!(
            Config::from_toml_str(&unconfigured)
                .expect("parse")
                .watched_download_folders,
            None,
            "an unconfigured list must round-trip as unconfigured, not as an empty one"
        );

        config.watched_download_folders = Some(Vec::new());
        let cleared = config.to_toml_string().expect("serialize");
        assert_eq!(
            Config::from_toml_str(&cleared)
                .expect("parse")
                .watched_download_folders,
            Some(Vec::new()),
            "an explicitly cleared list must round-trip as present and empty"
        );
        assert_ne!(
            unconfigured, cleared,
            "the two states must not serialize identically"
        );
    }

    #[test]
    fn a_configured_watch_list_is_trimmed_and_blanks_are_dropped() {
        let config = Config {
            watched_download_folders: Some(vec![
                "  /home/user/Downloads  ".to_owned(),
                String::new(),
                "   ".to_owned(),
                "/mnt/nas/mods".to_owned(),
            ]),
            ..Config::default()
        };
        assert_eq!(
            config.watched_download_folders(),
            vec![
                PathBuf::from("/home/user/Downloads"),
                PathBuf::from("/mnt/nas/mods"),
            ]
        );
    }

    #[test]
    fn an_explicitly_cleared_watch_list_resolves_to_nothing() {
        let config = Config {
            watched_download_folders: Some(Vec::new()),
            ..Config::default()
        };
        assert!(config.watched_download_folders().is_empty());
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

    #[test]
    fn a_credential_inside_an_array_of_tables_is_dropped_on_load() {
        // `[[section]]` is the natural TOML shape for "one entry per provider",
        // so it is exactly where somebody would paste a key. An array is not a
        // place the walk may stop.
        let text = format!(
            "{DEFAULT_CONFIG_EXAMPLE}\n[mods]\ndefault_catalog = \"curseforge\"\n\
             [[mods.sources]]\nname = \"private\"\napi_key = \"$2a$10$notreal\"\n"
        );
        let config = Config::from_toml_str(&text).expect("parse");
        assert!(config.find_secret_looking_key().is_none());
        let serialized = config.to_toml_string().expect("serialize");
        assert!(!serialized.contains("notreal"), "{serialized}");
        // Everything beside the credential survives, including the row it was
        // pasted into.
        assert_eq!(config.mods.default_catalog, "curseforge");
        assert!(serialized.contains("private"), "{serialized}");
    }

    #[test]
    fn a_credential_shaped_key_inside_an_array_of_tables_is_refused_on_save() {
        let mut config = Config::default();
        let mut row = toml::Table::new();
        row.insert("name".to_owned(), toml::Value::String("private".to_owned()));
        row.insert(
            "access_token".to_owned(),
            toml::Value::String("notreal".to_owned()),
        );
        config.mods.unknown.insert(
            "sources".to_owned(),
            toml::Value::Array(vec![toml::Value::Table(row)]),
        );

        let error = config
            .to_toml_string()
            .expect_err("a credential is refused");
        assert!(
            matches!(error, ConfigError::SecretRejected { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn a_credential_hand_edited_into_the_mods_table_is_dropped_on_load() {
        // `[mods]` is as open as every other table, so it is walked like every
        // other table. Preferring a catalog is a setting; reaching one is a
        // keyring credential (ADR 0004, ADR 0022).
        let text = format!(
            "{DEFAULT_CONFIG_EXAMPLE}\n[mods]\ndefault_catalog = \"curseforge\"\n\
             curseforge_api_key = \"$2a$10$notreal\"\n"
        );
        let config = Config::from_toml_str(&text).expect("parse");
        assert!(config.find_secret_looking_key().is_none());
        assert!(!config.mods.unknown.contains_key("curseforge_api_key"));
        // The setting beside it is untouched.
        assert_eq!(config.mods.default_catalog, "curseforge");
        assert!(
            !config
                .to_toml_string()
                .expect("serialize")
                .contains("notreal")
        );
    }

    #[test]
    fn a_credential_shaped_key_under_mods_is_refused_on_save() {
        let mut config = Config::default();
        config.mods.unknown.insert(
            "access_key".to_owned(),
            toml::Value::String("notreal".to_owned()),
        );
        // The key itself is redacted in the report: an untrusted key name can
        // carry credential material of its own.
        assert_eq!(
            config.find_secret_looking_key().as_deref(),
            Some("mods.<redacted>")
        );
        assert!(matches!(
            config.to_toml_string(),
            Err(ConfigError::SecretRejected { .. })
        ));
    }

    #[test]
    fn unknown_keys_inside_an_account_row_survive() {
        // An account row is written by whichever build signed the account in.
        // A newer one recording something here must survive an older one
        // running once, exactly like every other table.
        let text = format!(
            "{DEFAULT_CONFIG_EXAMPLE}\n\
             [accounts.2f8a1c40-0000-4000-8000-000000000001]\n\
             name = \"Steve\"\nkind = \"microsoft\"\n\
             added_at = \"2026-01-01T00:00:00Z\"\n\
             last_authenticated_at = \"2026-01-02T00:00:00Z\"\n\
             future_account_field = \"kept\"\n"
        );
        let config = Config::from_toml_str(&text).expect("parse");
        let account = config
            .accounts
            .get("2f8a1c40-0000-4000-8000-000000000001")
            .expect("account");
        assert_eq!(account.name, "Steve");
        assert_eq!(
            account
                .unknown
                .get("future_account_field")
                .and_then(toml::Value::as_str),
            Some("kept")
        );

        let round_tripped =
            Config::from_toml_str(&config.to_toml_string().expect("serialize")).expect("reparse");
        assert_eq!(round_tripped, config);
    }

    #[test]
    fn a_linked_pair_round_trips_with_keys_this_build_does_not_know() {
        let text = format!(
            "{DEFAULT_CONFIG_EXAMPLE}\n\
             [linked_pairs.2f8a1c40-0000-4000-8000-000000000001]\n\
             server = \"7c1e0b52-0000-4000-8000-000000000002\"\n\
             start_together = true\n\
             future_pair_field = \"kept\"\n"
        );
        let config = Config::from_toml_str(&text).expect("parse");
        let pair = config
            .linked_pairs
            .get("2f8a1c40-0000-4000-8000-000000000001")
            .expect("pair");
        assert_eq!(pair.server, "7c1e0b52-0000-4000-8000-000000000002");
        assert!(pair.start_together);

        let round_tripped =
            Config::from_toml_str(&config.to_toml_string().expect("serialize")).expect("reparse");
        assert_eq!(round_tripped, config);
        assert!(
            Config::from_toml_str(DEFAULT_CONFIG_EXAMPLE)
                .expect("parse")
                .linked_pairs
                .is_empty(),
            "a file written before pairs existed has none"
        );
    }

    #[test]
    fn a_credential_hand_edited_into_a_linked_pair_is_dropped_on_load() {
        let text = format!(
            "{DEFAULT_CONFIG_EXAMPLE}\n\
             [linked_pairs.2f8a1c40-0000-4000-8000-000000000001]\n\
             server = \"7c1e0b52-0000-4000-8000-000000000002\"\n\
             access_token = \"notreal\"\n"
        );
        let config = Config::from_toml_str(&text).expect("parse");
        let pair = config
            .linked_pairs
            .get("2f8a1c40-0000-4000-8000-000000000001")
            .expect("pair");
        assert!(pair.unknown.is_empty());
        assert_eq!(config.find_secret_looking_key(), None);
    }

    #[test]
    fn a_credential_hand_edited_into_an_account_row_is_dropped_on_load() {
        let text = format!(
            "{DEFAULT_CONFIG_EXAMPLE}\n\
             [accounts.2f8a1c40-0000-4000-8000-000000000001]\n\
             name = \"Steve\"\nkind = \"microsoft\"\n\
             added_at = \"2026-01-01T00:00:00Z\"\n\
             last_authenticated_at = \"2026-01-02T00:00:00Z\"\n\
             refresh_token = \"notreal\"\n"
        );
        let config = Config::from_toml_str(&text).expect("parse");
        let account = config
            .accounts
            .get("2f8a1c40-0000-4000-8000-000000000001")
            .expect("account");
        // The account itself is kept; only the credential is discarded.
        assert_eq!(account.name, "Steve");
        assert!(!account.unknown.contains_key("refresh_token"));
        assert!(config.find_secret_looking_key().is_none());
        assert!(
            !config
                .to_toml_string()
                .expect("serialize")
                .contains("notreal")
        );
    }

    #[test]
    fn credential_looking_dynamic_map_keys_are_never_persisted() {
        let text = format!(
            "{DEFAULT_CONFIG_EXAMPLE}\n\
             [minecraft_preferences.refresh_token_embedded_secret]\n\
             enabled = true\nvalue = \"notreal\"\n\
             [accounts.api_key_embedded_secret]\nname = \"notreal\"\n"
        );
        let loaded = Config::from_toml_str(&text).expect("parse");
        assert!(loaded.minecraft_preferences.is_empty());
        assert!(loaded.accounts.is_empty());
        let rewritten = loaded.to_toml_string().expect("serialize");
        assert!(!rewritten.contains("embedded_secret"));
        assert!(!rewritten.contains("notreal"));

        let mut config = Config::default();
        config.minecraft_preferences.insert(
            "refresh_token_embedded_secret".to_owned(),
            PreferenceSetting::managed("notreal"),
        );
        let error = config
            .to_toml_string()
            .expect_err("a dynamic credential key must be refused");
        assert!(matches!(error, ConfigError::SecretRejected { .. }));
        assert!(
            !error.to_string().contains("embedded_secret"),
            "the untrusted key must be redacted from the error"
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
    fn favorite_minecraft_versions_round_trip_in_the_order_they_were_given() {
        let mut config = Config::default();
        assert!(config.favorite_minecraft_versions.is_empty());
        // The order is the setting, so it has to survive the file rather than
        // being normalized into a set.
        config.favorite_minecraft_versions = vec![
            "1.21.1".to_owned(),
            "1.12.2".to_owned(),
            "1.20.1".to_owned(),
        ];

        let text = config.to_toml_string().expect("serialize");
        let parsed = Config::from_toml_str(&text).expect("re-parse");
        assert_eq!(
            parsed.favorite_minecraft_versions,
            ["1.21.1", "1.12.2", "1.20.1"]
        );
    }

    #[test]
    fn a_config_written_before_favorites_existed_reads_as_having_none() {
        let config = Config::from_toml_str(DEFAULT_CONFIG_EXAMPLE).expect("parse");
        assert!(config.favorite_minecraft_versions.is_empty());
    }

    #[test]
    fn favorites_do_not_displace_keys_this_build_does_not_know() {
        // Prepended, because the example ends inside a table and a key written
        // after it would belong to that table rather than to the document.
        let text = format!("from_a_newer_build = \"kept\"{DEFAULT_CONFIG_EXAMPLE}");
        let mut config = Config::from_toml_str(&text).expect("parse");
        config.favorite_minecraft_versions = vec!["1.21.1".to_owned()];
        let written = config.to_toml_string().expect("serialize");
        let parsed = Config::from_toml_str(&written).expect("re-parse");
        assert_eq!(parsed.favorite_minecraft_versions, ["1.21.1"]);
        assert_eq!(
            parsed
                .unknown
                .get("from_a_newer_build")
                .and_then(toml::Value::as_str),
            Some("kept")
        );
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
        config
            .minecraft_preferences
            .insert("gui_scale".to_owned(), PreferenceSetting::managed("auto"));
        let gui_scale = config
            .minecraft_preferences
            .get_mut("gui_scale")
            .expect("inserted preference");
        assert!(gui_scale.enabled);
        gui_scale.enabled = false;

        let text = config.to_toml_string().expect("serialize");
        let parsed = Config::from_toml_str(&text).expect("re-parse");
        assert!(!parsed.minecraft_preferences["gui_scale"].enabled);
        assert_eq!(parsed.minecraft_preferences["gui_scale"].value, "auto");
    }

    #[test]
    fn a_preference_id_from_a_newer_build_survives_a_load_and_save_cycle() {
        // An open map is what makes the preference surface grow without a
        // schema change, and the other half of that bargain is that an older
        // build must not discard an id it has no descriptor for.
        let text = "schema_version = 1\n\
             [minecraft_preferences.fullscreen]\n\
             enabled = true\n\
             value = \"true\"\n\
             [minecraft_preferences.a_setting_from_the_future]\n\
             enabled = true\n\
             value = \"17\"\n";
        let mut config = Config::from_toml_str(text).expect("parse");
        assert_eq!(config.minecraft_preferences.len(), 2);

        // Saving one known preference must not touch the other entry.
        config
            .minecraft_preferences
            .insert("fullscreen".to_owned(), PreferenceSetting::managed("false"));
        let round_tripped =
            Config::from_toml_str(&config.to_toml_string().expect("serialize")).expect("re-parse");

        assert_eq!(
            round_tripped.minecraft_preferences["a_setting_from_the_future"],
            PreferenceSetting::managed("17")
        );
        assert_eq!(
            round_tripped.minecraft_preferences["fullscreen"].value,
            "false"
        );
    }

    #[test]
    fn fresh_configuration_has_no_minecraft_preference_overrides() {
        let config = Config::default();
        assert!(config.minecraft_preferences.is_empty());

        let text = config.to_toml_string().expect("serialize");
        let parsed = Config::from_toml_str(&text).expect("re-parse");
        assert!(parsed.minecraft_preferences.is_empty());
    }
}
