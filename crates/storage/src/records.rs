//! The persisted shape of a client instance and a server.
//!
//! These records are written to two places that must agree: the SQLite index
//! and the instance's own `.launcher/*.json`. The index is a cache of
//! authority; the metadata file is what makes a lost database recoverable
//! (ADR 0002), which is why every field the launcher needs to reconstruct an
//! instance lives here and not only in a table.
//!
//! Provider-specific detail deliberately does not appear. A Modrinth pack's
//! version id or a CurseForge project id belongs in a provider structure under
//! `.launcher/source/`, not in the common model.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ephervex_model::{
    AttentionReason, ImportSourceKind, InstanceId, LoaderSelection, OperatorPermissionLevel,
    ServerId, ServerInstallation, StorageClass,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::NetworkAccess;

/// Re-exported so `ephervex_storage::JavaOverride` keeps resolving.
///
/// These definitions live in `ephervex-model` because storage and Java selection are independent
/// reader: `ephervex-java` resolves an override against the global settings and
/// cannot depend on the storage crate to do it.
pub use ephervex_model::{JavaOverride, JvmSettings};

/// Where an instance came from, and enough of a reference to say so in the UI.
///
/// The reference is an untranslated technical string - a file name, a pack
/// slug, a version id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportSource {
    pub kind: ImportSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

impl ImportSource {
    /// Created in the launcher rather than imported.
    #[must_use]
    pub const fn manual() -> Self {
        Self {
            kind: ImportSourceKind::Manual,
            reference: None,
        }
    }

    /// Whether this instance was created here rather than imported.
    #[must_use]
    pub fn is_manual(&self) -> bool {
        self.kind == ImportSourceKind::Manual
    }

    #[must_use]
    pub fn imported(kind: ImportSourceKind, reference: impl Into<String>) -> Self {
        Self {
            kind,
            reference: Some(reference.into()),
        }
    }
}

impl Default for ImportSource {
    fn default() -> Self {
        Self::manual()
    }
}

/// How one server binds to the network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerNetworkSettings {
    /// Off for test servers. Exposing an offline-mode server to a network is
    /// warned about rather than blocked.
    pub online_mode: bool,
    pub access: NetworkAccess,
    pub port: u16,
    /// Only meaningful for [`NetworkAccess::Custom`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_address: Option<String>,
}

impl Default for ServerNetworkSettings {
    fn default() -> Self {
        Self {
            online_mode: false,
            access: NetworkAccess::Localhost,
            port: 25565,
            bind_address: None,
        }
    }
}

impl ServerNetworkSettings {
    /// Whether this configuration exposes an unauthenticated server beyond the
    /// local machine, which the UI must warn about.
    #[must_use]
    pub const fn is_unauthenticated_and_exposed(&self) -> bool {
        !self.online_mode && !matches!(self.access, NetworkAccess::Localhost)
    }
}

/// A client instance as stored (`ClientInstance`).
///
/// `linked_server_ids` from the specification is deliberately *not* a field:
/// the link is owned by the server row, and a denormalized copy here would go
/// stale the moment a server is deleted. Ask the registry with
/// [`crate::Registry::linked_server_ids`] instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientRecord {
    pub id: InstanceId,
    /// User-facing and user-editable. Never used to build a path.
    pub name: String,
    pub root: PathBuf,
    pub storage: StorageClass,
    /// Untranslated Minecraft version id, e.g. `1.20.1`.
    pub minecraft_version: String,
    pub loader: LoaderSelection,
    #[serde(default)]
    pub source: ImportSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java_override: Option<JavaOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jvm_override: Option<JvmSettings>,
    /// Whether this instance's mods are the user's to move and take away,
    /// rather than a record of what a pack shipped.
    ///
    /// On for an instance created here, because there is no pack declaration
    /// to preserve: its mods were chosen by the person who made it. Off for an
    /// imported one, where the pack's declaration is a historical record and
    /// moving a mod away from it is a per-instance decision the importer never
    /// asked about.
    ///
    /// The serde default is `false` so a document written before this existed
    /// keeps the conservative reading; what a new instance starts with is
    /// decided where it is created, from its own source.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_pack_mod_version_changes: bool,
    /// The issue or bug report this instance reproduces, as an http(s) address
    /// exactly as the user gave it. Kept in the instance's metadata alone: the
    /// index has no column for it before 1.0.0 (ADR 0033).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_link: Option<String>,
    /// Why this instance is not straightforwardly runnable, if it is not. The
    /// launcher never presents an approximation as faithful.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attention: Vec<AttentionReason>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_launched_at: Option<DateTime<Utc>>,
}

impl ClientRecord {
    /// Whether this instance needs the user's attention before it can be
    /// trusted to reproduce anything.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        !self.attention.is_empty()
    }

    /// Records an attention reason, ignoring an exact duplicate so repeated
    /// reconciliation runs do not grow the list.
    pub fn add_attention(&mut self, reason: AttentionReason) {
        if !self.attention.contains(&reason) {
            self.attention.push(reason);
        }
    }
}

/// A server instance as stored (`ServerInstance`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRecord {
    pub id: ServerId,
    pub name: String,
    pub root: PathBuf,
    pub storage: StorageClass,
    /// The client this server was created from, if any. Several servers may
    /// point at one client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_client_id: Option<InstanceId>,
    pub minecraft_version: String,
    pub loader: LoaderSelection,
    /// A server is launchable only after its base installation has been fully
    /// published. Existing pre-installation records migrate to `NotInstalled`.
    #[serde(default)]
    pub installation: ServerInstallation,
    #[serde(default)]
    pub network: ServerNetworkSettings,
    /// Operator setup captured when this server was created. A missing profile
    /// keeps the intent pending until a suitable default account exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_setup: Option<ServerOperatorSetup>,
    #[serde(default)]
    pub source: ImportSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java_override: Option<JavaOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jvm_override: Option<JvmSettings>,
    /// Whether the user may change and remove the mods a pack declared for
    /// this server, as a client's setting of the same name allows. A server
    /// made here has no declaration to preserve and needs none. Kept in the
    /// server's metadata only: the index has no column for it before 1.0.0
    /// (ADR 0033).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_pack_mod_version_changes: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attention: Vec<AttentionReason>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_started_at: Option<DateTime<Utc>>,
}

/// A profile whose identity may be written to a Minecraft operator file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerOperatorProfile {
    pub id: Uuid,
    pub name: String,
    /// The identity form the server will assign to this player. This is
    /// intentionally independent from the kind of Ephervex account that
    /// supplied the player name.
    #[serde(alias = "kind")]
    pub identity: ServerOperatorIdentity,
}

/// How Minecraft authenticates the durable operator identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerOperatorIdentity {
    /// An authenticated profile UUID accepted by an online-mode server.
    #[serde(alias = "microsoft")]
    Online,
    /// Minecraft's canonical name-derived UUID for an offline-mode server.
    Offline,
}

/// Per-server operator intent copied from application defaults at creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerOperatorSetup {
    pub permission_level: OperatorPermissionLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ServerOperatorProfile>,
}

impl ServerRecord {
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        !self.attention.is_empty()
    }

    pub fn add_attention(&mut self, reason: AttentionReason) {
        if !self.attention.contains(&reason) {
            self.attention.push(reason);
        }
    }
}

/// The request to create a client instance.
///
/// Storage defaults to [`StorageClass::Temporary`] because that is the
/// product's defining default and a caller that forgets to choose should get
/// the disposable one, not the one that accumulates on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewClient {
    pub name: String,
    pub storage: StorageClass,
    pub minecraft_version: String,
    pub loader: LoaderSelection,
    pub source: ImportSource,
    pub attention: Vec<AttentionReason>,
    /// The issue address to record on the new instance, already validated by
    /// the caller.
    pub issue_link: Option<String>,
}

impl NewClient {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        minecraft_version: impl Into<String>,
        loader: LoaderSelection,
    ) -> Self {
        Self {
            name: name.into(),
            storage: StorageClass::default_for_new_instance(),
            minecraft_version: minecraft_version.into(),
            loader,
            source: ImportSource::manual(),
            attention: Vec::new(),
            issue_link: None,
        }
    }

    #[must_use]
    pub const fn with_storage(mut self, storage: StorageClass) -> Self {
        self.storage = storage;
        self
    }

    #[must_use]
    pub fn with_source(mut self, source: ImportSource) -> Self {
        self.source = source;
        self
    }

    #[must_use]
    pub fn with_attention(mut self, attention: Vec<AttentionReason>) -> Self {
        self.attention = attention;
        self
    }

    #[must_use]
    pub fn with_issue_link(mut self, issue_link: impl Into<String>) -> Self {
        self.issue_link = Some(issue_link.into());
        self
    }
}

/// The request to create a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerCreationSource {
    /// An independently selected exact Minecraft and loader combination.
    Standalone {
        minecraft_version: String,
        loader: LoaderSelection,
    },
    /// Derive the exact selection from an existing client record.
    LinkedClient { client_id: InstanceId },
}

/// The request to create a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewServer {
    pub name: String,
    pub storage: StorageClass,
    pub creation: ServerCreationSource,
    pub network: ServerNetworkSettings,
    pub port_selection: ServerPortSelection,
    pub source: ImportSource,
    pub attention: Vec<AttentionReason>,
}

/// Whether core should choose a free port or preserve the caller's exact port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerPortSelection {
    #[default]
    Automatic,
    Explicit(u16),
}

impl NewServer {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        minecraft_version: impl Into<String>,
        loader: LoaderSelection,
    ) -> Self {
        Self {
            name: name.into(),
            storage: StorageClass::default_for_new_instance(),
            creation: ServerCreationSource::Standalone {
                minecraft_version: minecraft_version.into(),
                loader,
            },
            network: ServerNetworkSettings::default(),
            port_selection: ServerPortSelection::Automatic,
            source: ImportSource::manual(),
            attention: Vec::new(),
        }
    }

    #[must_use]
    pub const fn with_storage(mut self, storage: StorageClass) -> Self {
        self.storage = storage;
        self
    }

    /// Links this server to a client instance.
    #[must_use]
    pub fn linked_to(mut self, client: InstanceId) -> Self {
        self.creation = ServerCreationSource::LinkedClient { client_id: client };
        self
    }

    /// Creates a linked-base request without accepting a redundant version
    /// selection from the caller.
    #[must_use]
    pub fn linked(name: impl Into<String>, client_id: InstanceId) -> Self {
        Self {
            name: name.into(),
            storage: StorageClass::default_for_new_instance(),
            creation: ServerCreationSource::LinkedClient { client_id },
            network: ServerNetworkSettings::default(),
            port_selection: ServerPortSelection::Automatic,
            source: ImportSource::manual(),
            attention: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_network(mut self, network: ServerNetworkSettings) -> Self {
        self.port_selection = ServerPortSelection::Explicit(network.port);
        self.network = network;
        self
    }

    #[must_use]
    pub fn with_source(mut self, source: ImportSource) -> Self {
        self.source = source;
        self
    }
}

/// The suffix a duplicate's name gets when the caller does not supply one.
///
/// User-facing but deliberately *not* localized: an instance name is user data
/// that ends up in bug reports and in a directory listing, and a name that
/// changes meaning with the interface language is not data.
pub const DUPLICATE_NAME_SUFFIX: &str = " (copy)";

/// The request to duplicate a client instance.
///
/// Both optional fields default to "the same as the original", which is what
/// `ephervex instance duplicate <id>` with no other argument should mean.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DuplicateClient {
    /// The copy's name, or `None` for the original's plus
    /// [`DUPLICATE_NAME_SUFFIX`].
    pub name: Option<String>,
    /// Where the copy lives, or `None` for the original's storage class.
    ///
    /// The caller must choose how to handle a persistent
    /// instance to try something disposable is the common case.
    pub storage: Option<StorageClass>,
}

impl DuplicateClient {
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            storage: None,
        }
    }

    #[must_use]
    pub const fn with_storage(mut self, storage: StorageClass) -> Self {
        self.storage = Some(storage);
        self
    }
}

/// What duplicating an instance produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicatedClient {
    pub record: ClientRecord,
    /// Symlinked directories the copy did not follow, relative to the source
    /// root. Surfaced so a frontend can say the copy is not byte-identical
    /// rather than leaving the user to find out.
    pub skipped_links: Vec<PathBuf>,
}

/// Whether deleting a record should also delete its files.
///
/// Never inferred. Explicit user-initiated deletion is the only thing that
/// removes an instance's contents (ADR 0006).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileDisposition {
    /// Remove the index row and the instance directory.
    Remove,
    /// Remove the index row only, leaving the directory on disk. Used by
    /// reconciliation and by "forget without deleting".
    Keep,
}

/// A canonical mutation that committed even when something after the commit
/// point did not fully succeed, with the conditions to report to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit<T, W> {
    pub record: T,
    pub warnings: Vec<W>,
}

impl<T, W> Commit<T, W> {
    #[must_use]
    pub const fn clean(record: T) -> Self {
        Self {
            record,
            warnings: Vec::new(),
        }
    }
}

/// A canonical server mutation that committed even if its SQLite mirror did not.
pub type ServerCommit<T> = Commit<T, ServerCommitWarning>;

/// A canonical client mutation that committed, with the conditions to report.
pub type ClientCommit<T> = Commit<T, ClientCommitWarning>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerCommitWarning {
    /// Metadata or root publication succeeded, but the query index needs
    /// reconciliation from the canonical record.
    IndexOutOfSync { detail: String },
    /// The canonical metadata file was published and is visible, but syncing
    /// its directory failed, so the mutation may not survive a power loss.
    MetadataNotDurable { detail: String },
}

/// A condition on a client mutation that committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientCommitWarning {
    /// The canonical metadata was published, but mirroring it into the query
    /// index failed. Reconciliation adopts the canonical record.
    IndexOutOfSync { detail: String },
    /// The canonical metadata file was published and is visible, but syncing
    /// its directory failed, so the mutation may not survive a power loss.
    MetadataNotDurable { detail: String },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use ephervex_model::LoaderKind;

    #[test]
    fn new_instances_are_temporary_unless_asked_otherwise() {
        // The product's defining default: forgetting to choose gives you the
        // disposable one.
        let request = NewClient::new("repro", "1.20.1", LoaderSelection::vanilla());
        assert_eq!(request.storage, StorageClass::Temporary);
        assert_eq!(
            request.with_storage(StorageClass::Persistent).storage,
            StorageClass::Persistent
        );
    }

    #[test]
    fn an_offline_server_reachable_beyond_localhost_is_flagged() {
        let mut network = ServerNetworkSettings::default();
        assert!(!network.is_unauthenticated_and_exposed());

        network.access = NetworkAccess::Lan;
        assert!(network.is_unauthenticated_and_exposed());

        network.online_mode = true;
        assert!(!network.is_unauthenticated_and_exposed());
    }

    #[test]
    fn explicit_server_ports_are_distinct_from_automatic_defaults() {
        let automatic = NewServer::new("automatic", "1.20.1", LoaderSelection::vanilla());
        assert_eq!(automatic.port_selection, ServerPortSelection::Automatic);

        let explicit = NewServer::new("custom", "1.20.1", LoaderSelection::vanilla()).with_network(
            ServerNetworkSettings {
                port: 30_000,
                ..ServerNetworkSettings::default()
            },
        );
        assert_eq!(
            explicit.port_selection,
            ServerPortSelection::Explicit(30_000)
        );
        assert_eq!(explicit.network.port, 30_000);
    }

    #[test]
    fn legacy_operator_account_kind_deserializes_as_server_identity() {
        let id = Uuid::new_v4();
        let profile: ServerOperatorProfile = serde_json::from_str(&format!(
            r#"{{"id":"{id}","name":"Developer","kind":"microsoft"}}"#
        ))
        .expect("legacy profile");
        assert_eq!(profile.identity, ServerOperatorIdentity::Online);

        let rendered = serde_json::to_string(&profile).expect("current profile");
        assert!(rendered.contains(r#""identity":"online""#));
        assert!(!rendered.contains(r#""kind""#));
    }

    #[test]
    fn repeated_attention_reasons_are_not_accumulated() {
        // Reconciliation runs on every startup; a warning list that grows by
        // one entry per launch would be useless.
        let mut record = sample_client();
        record.add_attention(AttentionReason::MissingRoot);
        record.add_attention(AttentionReason::MissingRoot);
        assert_eq!(record.attention, vec![AttentionReason::MissingRoot]);
        assert!(record.needs_attention());
    }

    #[test]
    fn records_round_trip_through_json_which_is_how_recovery_works() {
        let record = sample_client();
        let json = serde_json::to_string(&record).expect("serialize");
        let back: ClientRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(record, back);
    }

    fn sample_client() -> ClientRecord {
        let now = Utc::now();
        ClientRecord {
            id: InstanceId::new(),
            name: "Crash repro".to_owned(),
            root: PathBuf::from("/tmp/ephervex/instances/x"),
            storage: StorageClass::Temporary,
            minecraft_version: "1.20.1".to_owned(),
            loader: LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
            source: ImportSource::imported(ImportSourceKind::ModrinthPack, "report.mrpack"),
            java_override: None,
            jvm_override: Some(JvmSettings {
                max_memory_mib: Some(6144),
                ..JvmSettings::default()
            }),
            allow_pack_mod_version_changes: false,
            issue_link: Some("https://example.invalid/issues/42".to_owned()),
            attention: Vec::new(),
            created_at: now,
            updated_at: now,
            last_launched_at: None,
        }
    }
}
