//! Local, user-managed safety snapshots of installed clients and servers.
//!
//! A snapshot captures runnable reproduction state, not an export archive. It
//! lives under the host root, never contains snapshot history, and preserves
//! symlinks instead of following them into shared stores.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ephervex_model::{LoaderSelection, SnapshotId};
use ephervex_storage::{
    ClientCommitWarning, ClientRecord, CloneMethod, ServerCommitWarning, ServerRecord, clone_file,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio_util::sync::CancellationToken;

use crate::{App, CoreResult, InstalledHost, IsolationError, ModPresence};

const SNAPSHOT_VERSION: u32 = 1;
const SNAPSHOT_MANIFEST: &str = "snapshot.json";
const MAX_SNAPSHOT_LABEL_CHARS: usize = 120;

/// Why a snapshot was created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotOrigin {
    Manual,
    BeforeRestore,
    BeforeIsolation,
    /// An origin written by a newer Ephervex, retained verbatim.
    Unknown(String),
}

impl SnapshotOrigin {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Manual => "manual",
            Self::BeforeRestore => "before_restore",
            Self::BeforeIsolation => "before_isolation",
            Self::Unknown(value) => value,
        }
    }
}

impl Serialize for SnapshotOrigin {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SnapshotOrigin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "manual" => Self::Manual,
            "before_restore" => Self::BeforeRestore,
            "before_isolation" => Self::BeforeIsolation,
            _ => Self::Unknown(value),
        })
    }
}

/// Metadata shown without opening the captured tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRecord {
    pub id: SnapshotId,
    pub label: String,
    pub captured_at: DateTime<Utc>,
    pub host_kind: String,
    pub host_id: String,
    pub minecraft_version: String,
    pub loader: LoaderSelection,
    pub enabled_mods: usize,
    pub disabled_mods: usize,
    pub copy_method: CloneMethod,
    pub apparent_bytes: u64,
    pub origin: SnapshotOrigin,
}

/// Restore policy. A safety snapshot is on by default because restore replaces
/// rather than merges live state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotRestoreOptions {
    pub safety_snapshot: bool,
}

impl Default for SnapshotRestoreOptions {
    fn default() -> Self {
        Self {
            safety_snapshot: true,
        }
    }
}

/// What a completed restoration applied and how to get back to the prior state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRestore {
    pub restored: SnapshotRecord,
    pub safety_snapshot: Option<SnapshotRecord>,
    /// Conditions on the restored client metadata the user should hear about.
    pub warnings: Vec<ClientCommitWarning>,
    /// Conditions on the restored server metadata the user should hear about.
    pub server_warnings: Vec<ServerCommitWarning>,
}

/// The record a restore addresses, read once before the restore publishes.
///
/// Publishing marks the host's root incomplete, and a record that follows
/// files a move left at another root is not followed to an incomplete root
/// (ADR 0025). Reading the record again afterwards would name the superseded
/// root the restore never wrote to.
enum RestoreTarget {
    Client {
        record: ClientRecord,
        /// The host's own metadata as it stood before the restore, whose
        /// issue link a restored snapshot must not replace. Absent when that
        /// metadata could not be read, which leaves the snapshot's link alone.
        canonical: Option<ClientRecord>,
        /// The temporary tree the record's persistent root supersedes, which
        /// is removed once the restore is final.
        superseded: Option<PathBuf>,
    },
    Server {
        record: ServerRecord,
        /// The temporary tree the record's persistent root supersedes, which
        /// is removed once the restore is final.
        superseded: Option<PathBuf>,
    },
}

impl RestoreTarget {
    fn read(app: &App, host: InstalledHost) -> CoreResult<Self> {
        Ok(match host {
            InstalledHost::Instance(id) => Self::Client {
                record: app.client(id)?,
                // Unreadable metadata must not block the restore that replaces it.
                canonical: app.current_client(id).ok(),
                superseded: app.registry().superseded_client_root(id)?,
            },
            InstalledHost::Server(id) => Self::Server {
                record: app.server(id)?,
                superseded: app.registry().superseded_server_root(id)?,
            },
        })
    }

    fn root(&self) -> &Path {
        match self {
            Self::Client { record, .. } => &record.root,
            Self::Server { record, .. } => &record.root,
        }
    }
}

/// What recording a restored host's metadata replaced and reported.
struct SynchronizedHost {
    /// For a server, the record it replaced.
    previous_server: Option<ServerRecord>,
    /// The temporary tree the restored root supersedes.
    superseded: Option<PathBuf>,
    client_warnings: Vec<ClientCommitWarning>,
    server_warnings: Vec<ServerCommitWarning>,
}

#[derive(Serialize, Deserialize)]
struct SnapshotDocument {
    snapshot_version: u32,
    #[serde(default)]
    written_by: String,
    #[serde(flatten)]
    record: SnapshotRecord,
}

impl SnapshotDocument {
    fn new(record: SnapshotRecord) -> Self {
        Self {
            snapshot_version: SNAPSHOT_VERSION,
            written_by: format!(
                "{} {}",
                ephervex_model::APP_NAME,
                ephervex_model::APP_VERSION
            ),
            record,
        }
    }
}

/// Failures from snapshot management.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("{host} is running")]
    HostRunning { host: String },
    #[error("{path} is being restored or moved by another operation")]
    HostBusy { path: PathBuf },
    #[error("an isolation session owns the host's mods")]
    IsolationActive,
    #[error("snapshot {id} does not exist")]
    NotFound { id: SnapshotId },
    #[error("snapshot {snapshot} belongs to {found}, not {expected}")]
    HostMismatch {
        snapshot: SnapshotId,
        expected: String,
        found: String,
    },
    #[error("that snapshot label is invalid")]
    LabelInvalid,
    #[error("snapshot state at {path} could not be read")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("snapshot state at {path} could not be written")]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("snapshot metadata at {path} is invalid")]
    Invalid {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("snapshot metadata at {path} has version {found}, newer than {supported}")]
    UnsupportedVersion {
        path: PathBuf,
        found: u32,
        supported: u32,
    },
    #[error("snapshot operation was canceled")]
    Canceled,
}

impl SnapshotError {
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::HostRunning { .. } => "error-snapshot-host-running",
            Self::HostBusy { .. } => "error-snapshot-host-busy",
            Self::IsolationActive => "error-snapshot-isolation-active",
            Self::NotFound { .. } => "error-snapshot-not-found",
            Self::HostMismatch { .. } => "error-snapshot-host-mismatch",
            Self::LabelInvalid => "error-snapshot-label-invalid",
            Self::Unreadable { .. } => "error-snapshot-unreadable",
            Self::Unwritable { .. } => "error-snapshot-unwritable",
            Self::Invalid { .. } => "error-snapshot-invalid",
            Self::UnsupportedVersion { .. } => "error-snapshot-version-newer",
            Self::Canceled => "error-snapshot-canceled",
        }
    }

    #[must_use]
    pub fn message_args(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::HostRunning { host } => vec![("host", host.clone())],
            Self::NotFound { id } => vec![("snapshot", id.to_string())],
            Self::HostMismatch {
                snapshot,
                expected,
                found,
            } => vec![
                ("snapshot", snapshot.to_string()),
                ("expected", expected.clone()),
                ("found", found.clone()),
            ],
            Self::UnsupportedVersion {
                found, supported, ..
            } => vec![
                ("found", found.to_string()),
                ("supported", supported.to_string()),
            ],
            _ => Vec::new(),
        }
    }

    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::HostBusy { path }
            | Self::Unreadable { path, .. }
            | Self::Unwritable { path, .. }
            | Self::Invalid { path, .. }
            | Self::UnsupportedVersion { path, .. } => Some(path.clone()),
            _ => None,
        }
    }

    #[must_use]
    pub const fn is_cancellation(&self) -> bool {
        matches!(self, Self::Canceled)
    }
}

impl App {
    /// Captures the current state of a stopped client or local server.
    ///
    /// The host stays claimed for the whole capture, so a launch cannot begin
    /// while its files are being read.
    pub fn create_snapshot(
        &self,
        host: InstalledHost,
        label: &str,
        origin: SnapshotOrigin,
        cancel: &CancellationToken,
    ) -> Result<SnapshotRecord, SnapshotError> {
        check_canceled(cancel)?;
        let _claim = self.claim_snapshot_host(host)?;
        self.create_snapshot_holding_the_host(host, label, origin, cancel)
    }

    /// Claims a host for a snapshot operation.
    fn claim_snapshot_host(
        &self,
        host: InstalledHost,
    ) -> Result<ephervex_storage::HostClaim, SnapshotError> {
        self.registry()
            .claim_host(host.record_ref())
            .map_err(|error| match error {
                ephervex_storage::RegistryError::HostBusy { path } => {
                    SnapshotError::HostBusy { path }
                }
                other => snapshot_core_error(crate::CoreError::from(other)),
            })
    }

    /// The capture itself, for a caller that holds the host's claim.
    fn create_snapshot_holding_the_host(
        &self,
        host: InstalledHost,
        label: &str,
        origin: SnapshotOrigin,
        cancel: &CancellationToken,
    ) -> Result<SnapshotRecord, SnapshotError> {
        check_canceled(cancel)?;
        self.refuse_snapshot_while_running(host)?;
        let label = snapshot_label(label)?;
        let layout = self.host_layout(host).map_err(snapshot_core_error)?;
        let id = SnapshotId::new();
        let staging =
            ephervex_storage::metadata::staging_dir(&layout.root).join(format!("snapshot-{id}"));
        let destination =
            ephervex_storage::metadata::snapshots_dir(&layout.root).join(id.to_string());
        std::fs::create_dir_all(staging.parent().unwrap_or(&layout.root)).map_err(|source| {
            SnapshotError::Unwritable {
                path: staging.clone(),
                source,
            }
        })?;
        std::fs::create_dir_all(destination.parent().unwrap_or(&layout.root)).map_err(
            |source| SnapshotError::Unwritable {
                path: destination.clone(),
                source,
            },
        )?;
        std::fs::create_dir(&staging).map_err(|source| SnapshotError::Unwritable {
            path: staging.clone(),
            source,
        })?;

        let result = self
            .capture_snapshot(
                SnapshotCapture {
                    host,
                    layout: &layout,
                    id,
                    label,
                    origin,
                    staging: &staging,
                },
                cancel,
            )
            .and_then(|record| {
                check_canceled(cancel)?;
                Ok(record)
            });
        let record = match result {
            Ok(record) => record,
            Err(error) => {
                ephervex_storage::remove_tree_if_present(&staging).map_err(|source| {
                    SnapshotError::Unwritable {
                        path: staging.clone(),
                        source: std::io::Error::other(format!("{error}; cleanup failed: {source}")),
                    }
                })?;
                ephervex_storage::sync_parent(&staging).map_err(|source| {
                    SnapshotError::Unwritable {
                        path: staging.clone(),
                        source,
                    }
                })?;
                return Err(error);
            }
        };
        std::fs::rename(&staging, &destination).map_err(|source| SnapshotError::Unwritable {
            path: destination.clone(),
            source,
        })?;
        ephervex_storage::sync_parent(&destination).map_err(|source| {
            SnapshotError::Unwritable {
                path: destination,
                source,
            }
        })?;
        Ok(record)
    }

    /// Lists snapshots newest first.
    pub fn snapshots(&self, host: InstalledHost) -> Result<Vec<SnapshotRecord>, SnapshotError> {
        let layout = self.host_layout(host).map_err(snapshot_core_error)?;
        let directory = ephervex_storage::metadata::snapshots_dir(&layout.root);
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(SnapshotError::Unreadable {
                    path: directory,
                    source,
                });
            }
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| SnapshotError::Unreadable {
                path: directory.clone(),
                source,
            })?;
            if entry
                .file_type()
                .map_err(|source| SnapshotError::Unreadable {
                    path: entry.path(),
                    source,
                })?
                .is_dir()
            {
                records.push(read_manifest(&entry.path())?);
            }
        }
        records.sort_by(|left, right| {
            right
                .captured_at
                .cmp(&left.captured_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(records)
    }

    /// Changes only the user-facing label.
    pub fn rename_snapshot(
        &self,
        host: InstalledHost,
        id: SnapshotId,
        label: &str,
    ) -> Result<SnapshotRecord, SnapshotError> {
        let label = snapshot_label(label)?;
        let directory = self.snapshot_directory(host, id)?;
        let mut record = read_manifest(&directory)?;
        record.label = label;
        write_manifest(&directory, &record)?;
        Ok(record)
    }

    /// Deletes one explicitly selected snapshot. The safety snapshot of an
    /// active isolation session is refused, since cancel restores from it.
    pub fn delete_snapshot(&self, host: InstalledHost, id: SnapshotId) -> CoreResult<()> {
        let directory = self.snapshot_directory(host, id)?;
        read_manifest(&directory)?;
        if let Some(session) = self.isolation_state(host)?
            && session.safety_snapshot == id
        {
            return Err(IsolationError::SafetySnapshotInUse { snapshot: id }.into());
        }
        if let InstalledHost::Server(server) = host
            && self
                .isolation_leader(server)?
                .is_some_and(|leader| leader.safety_snapshot == id)
        {
            return Err(IsolationError::SafetySnapshotInUse { snapshot: id }.into());
        }
        ephervex_storage::remove_tree_if_present(&directory).map_err(|source| {
            SnapshotError::Unwritable {
                path: directory.clone(),
                source,
            }
        })?;
        ephervex_storage::sync_parent(&directory).map_err(|source| SnapshotError::Unwritable {
            path: directory,
            source,
        })?;
        Ok(())
    }

    /// Replaces the live host parts with a captured snapshot.
    ///
    /// Refused while an isolation session is active: the restore replaces the
    /// mods the session names its candidates from.
    pub fn restore_snapshot(
        &self,
        host: InstalledHost,
        id: SnapshotId,
        options: SnapshotRestoreOptions,
        cancel: &CancellationToken,
    ) -> Result<SnapshotRestore, SnapshotError> {
        self.restore_snapshot_for(host, id, options, cancel, SnapshotRestoreOwner::Caller)
    }

    /// Restores the safety snapshot an isolation session took, which is how
    /// the session itself ends.
    pub(crate) fn restore_isolation_snapshot(
        &self,
        host: InstalledHost,
        id: SnapshotId,
        cancel: &CancellationToken,
    ) -> Result<SnapshotRestore, SnapshotError> {
        self.restore_snapshot_for(
            host,
            id,
            SnapshotRestoreOptions {
                safety_snapshot: false,
            },
            cancel,
            SnapshotRestoreOwner::IsolationSession,
        )
    }

    fn restore_snapshot_for(
        &self,
        host: InstalledHost,
        id: SnapshotId,
        options: SnapshotRestoreOptions,
        cancel: &CancellationToken,
        owner: SnapshotRestoreOwner,
    ) -> Result<SnapshotRestore, SnapshotError> {
        // Held until the restore is finished, rolled back, or abandoned: it
        // keeps a move from carrying the host away mid-restore, keeps a launch
        // from starting between the running check and the swap, and tells
        // reconciliation in another launcher that this restore is running.
        let claim = self.claim_snapshot_host(host)?;
        self.refuse_snapshot_while_running(host)?;
        // The restore replaces the recipe wholesale, so a background write
        // that read the previous one must not land on top of it.
        let _gate =
            crate::host_recipe_gate::hold_blocking_for_change(self.registry(), host, cancel)
                .map_err(snapshot_core_error)?;
        self.refuse_restore_during_isolation(host, owner)?;
        let target = RestoreTarget::read(self, host).map_err(snapshot_core_error)?;
        let layout = crate::mods::HostLayout::at(host, target.root().to_path_buf())
            .map_err(snapshot_core_error)?;
        let snapshot = self.snapshot_directory(host, id)?;
        let restored = read_manifest(&snapshot)?;
        let expected = format!("{}:{}", host.kind(), host.id());
        let found = format!("{}:{}", restored.host_kind, restored.host_id);
        if restored.host_kind != host.kind() || restored.host_id != host.id() {
            return Err(SnapshotError::HostMismatch {
                snapshot: id,
                expected,
                found,
            });
        }

        let safety_snapshot = if options.safety_snapshot {
            Some(self.create_snapshot_holding_the_host(
                host,
                "Before snapshot restore",
                SnapshotOrigin::BeforeRestore,
                cancel,
            )?)
        } else {
            None
        };
        check_canceled(cancel)?;
        if matches!(host, InstalledHost::Server(_)) {
            // Saved into the files this replaces; the next start would write them over the restore.
            crate::server::discard_kept_properties(&layout.root).map_err(snapshot_core_error)?;
        }

        let parts = restore_parts(host, &layout.root, &snapshot)?;
        let transaction = ephervex_storage::prepare_restore(&layout.root, id, parts.clone())
            .map_err(|source| SnapshotError::Unwritable {
                path: ephervex_storage::metadata::staging_dir(&layout.root),
                source,
            })?;
        if let Err(error) = stage_restore_parts(&transaction, &snapshot, &parts, cancel) {
            if let Err(abort) = transaction.abort() {
                return Err(SnapshotError::Unwritable {
                    path: transaction.directory().to_path_buf(),
                    source: std::io::Error::other(format!(
                        "could not stage snapshot restore: {error}; cleanup failed: {abort}"
                    )),
                });
            }
            return Err(error);
        }
        transaction
            .publish()
            .map_err(|source| SnapshotError::Unwritable {
                path: transaction.directory().to_path_buf(),
                source,
            })?;

        let SynchronizedHost {
            previous_server,
            superseded,
            client_warnings: warnings,
            server_warnings,
        } = match self.synchronize_restored_host(&claim, target) {
            Ok(synchronized) => synchronized,
            Err(error) => {
                let rollback = transaction.rollback();
                let message = match rollback {
                    Ok(()) => error.to_string(),
                    Err(rollback) => {
                        format!(
                            "could not record restored host: {error}; rollback failed: {rollback}"
                        )
                    }
                };
                return Err(SnapshotError::Unwritable {
                    path: transaction.directory().to_path_buf(),
                    source: std::io::Error::other(message),
                });
            }
        };
        transaction
            .finish()
            .map_err(|source| SnapshotError::Unwritable {
                path: transaction.directory().to_path_buf(),
                source,
            })?;
        self.settle_restored_host(
            host,
            previous_server.as_ref(),
            superseded.as_deref(),
            &claim,
        );
        Ok(SnapshotRestore {
            restored,
            safety_snapshot,
            warnings,
            server_warnings,
        })
    }

    /// Brings what depends on the restored host in line once the restore can
    /// no longer be rolled back: the client's server list follows the
    /// records, and a temporary tree the restored root superseded is removed.
    fn settle_restored_host(
        &self,
        host: InstalledHost,
        previous_server: Option<&ServerRecord>,
        superseded: Option<&Path>,
        claim: &ephervex_storage::HostClaim,
    ) {
        match host {
            InstalledHost::Instance(id) => {
                self.sync_linked_server_lists_of_client(id);
                if let Some(stale) = superseded {
                    self.registry()
                        .discard_superseded_client_root(stale, id, claim);
                }
            }
            InstalledHost::Server(id) => {
                if let Ok(restored_server) = self.server(id) {
                    self.sync_linked_server_list(&restored_server, previous_server);
                }
                if let Some(stale) = superseded {
                    self.registry()
                        .discard_superseded_server_root(stale, id, claim);
                }
            }
        }
    }

    fn capture_snapshot(
        &self,
        capture: SnapshotCapture<'_>,
        cancel: &CancellationToken,
    ) -> Result<SnapshotRecord, SnapshotError> {
        let (method, apparent_bytes) = capture_files(&capture, cancel)?;

        let (minecraft_version, loader) = match capture.host {
            InstalledHost::Instance(id) => {
                let record = self.current_client(id).map_err(snapshot_core_error)?;
                (record.minecraft_version, record.loader)
            }
            InstalledHost::Server(id) => {
                let record = self.server(id).map_err(snapshot_core_error)?;
                (record.minecraft_version, record.loader)
            }
        };
        let mods = match capture.host {
            InstalledHost::Instance(id) => self.managed_mod_snapshot(id),
            InstalledHost::Server(id) => self.server_mod_snapshot(id),
        }
        .map_err(snapshot_core_error)?;
        let enabled_mods = mods
            .mods
            .iter()
            .filter(|entry| entry.presence == ModPresence::Installed)
            .count();
        let disabled_mods = mods
            .mods
            .iter()
            .filter(|entry| entry.presence == ModPresence::Disabled)
            .count();
        let record = SnapshotRecord {
            id: capture.id,
            label: capture.label,
            captured_at: Utc::now(),
            host_kind: capture.host.kind().to_owned(),
            host_id: capture.host.id(),
            minecraft_version,
            loader,
            enabled_mods,
            disabled_mods,
            copy_method: method,
            apparent_bytes,
            origin: capture.origin,
        };
        write_manifest(capture.staging, &record)?;
        Ok(record)
    }

    /// Checked under the host's gate, because a session starts under it too.
    fn refuse_restore_during_isolation(
        &self,
        host: InstalledHost,
        owner: SnapshotRestoreOwner,
    ) -> Result<(), SnapshotError> {
        let held = self.isolation_holds(host).map_err(snapshot_core_error)?;
        if held && owner == SnapshotRestoreOwner::Caller {
            Err(SnapshotError::IsolationActive)
        } else {
            Ok(())
        }
    }

    fn refuse_snapshot_while_running(&self, host: InstalledHost) -> Result<(), SnapshotError> {
        let running = match host {
            InstalledHost::Instance(id) => self
                .launch_state(id)
                .map_err(snapshot_core_error)?
                .is_some(),
            InstalledHost::Server(id) => self
                .server_status(id)
                .map_err(snapshot_core_error)?
                .runtime
                .may_be_running(),
        };
        if running {
            Err(SnapshotError::HostRunning { host: host.id() })
        } else {
            Ok(())
        }
    }

    fn snapshot_directory(
        &self,
        host: InstalledHost,
        id: SnapshotId,
    ) -> Result<PathBuf, SnapshotError> {
        let layout = self.host_layout(host).map_err(snapshot_core_error)?;
        let directory =
            ephervex_storage::metadata::snapshots_dir(&layout.root).join(id.to_string());
        if directory.is_dir() {
            Ok(directory)
        } else {
            Err(SnapshotError::NotFound { id })
        }
    }

    /// Records the restored host's metadata at the root the restore wrote to,
    /// which also repairs an index row that still names a superseded root. For
    /// a server, returns the record it replaced, which tells the linked
    /// client's server list what the entry showed before. Also returns the
    /// warnings on the committed metadata.
    fn synchronize_restored_host(
        &self,
        claim: &ephervex_storage::HostClaim,
        target: RestoreTarget,
    ) -> Result<SynchronizedHost, SnapshotError> {
        match target {
            RestoreTarget::Client {
                record: current,
                canonical,
                superseded,
            } => {
                let path = ephervex_storage::metadata::client_metadata_path(&current.root);
                let mut restored = ephervex_storage::metadata::read_client_metadata(&current.root)
                    .map_err(|error| SnapshotError::Unreadable {
                        path: path.clone(),
                        source: std::io::Error::other(error),
                    })?;
                if restored.id != current.id {
                    return Err(SnapshotError::Unreadable {
                        path,
                        source: std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "restored client metadata has a different identity",
                        ),
                    });
                }
                restored.root = current.root;
                restored.storage = current.storage;
                if let Some(canonical) = canonical {
                    restored.issue_link = canonical.issue_link;
                }
                let warnings = self
                    .registry()
                    .update_client_committed_claimed(&mut restored, claim)
                    .map_err(crate::CoreError::from)
                    .map_err(snapshot_core_error)?;
                Ok(SynchronizedHost {
                    previous_server: None,
                    superseded,
                    client_warnings: warnings,
                    server_warnings: Vec::new(),
                })
            }
            RestoreTarget::Server {
                record: current,
                superseded,
            } => {
                let path = ephervex_storage::metadata::server_metadata_path(&current.root);
                let mut restored = ephervex_storage::metadata::read_server_metadata(&current.root)
                    .map_err(|error| SnapshotError::Unreadable {
                        path: path.clone(),
                        source: std::io::Error::other(error),
                    })?;
                if restored.id != current.id {
                    return Err(SnapshotError::Unreadable {
                        path,
                        source: std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "restored server metadata has a different identity",
                        ),
                    });
                }
                restored.root.clone_from(&current.root);
                restored.storage = current.storage;
                let committed = self
                    .registry()
                    .update_server_committed_claimed(&mut restored, claim)
                    .map_err(crate::CoreError::from)
                    .map_err(snapshot_core_error)?;
                Ok(SynchronizedHost {
                    previous_server: Some(current),
                    superseded,
                    client_warnings: Vec::new(),
                    server_warnings: committed.warnings,
                })
            }
        }
    }
}

struct SnapshotCapture<'a> {
    host: InstalledHost,
    layout: &'a crate::mods::HostLayout,
    id: SnapshotId,
    label: String,
    origin: SnapshotOrigin,
    staging: &'a Path,
}

fn capture_files(
    capture: &SnapshotCapture<'_>,
    cancel: &CancellationToken,
) -> Result<(CloneMethod, u64), SnapshotError> {
    let mut method = CloneMethod::Reflink;
    let mut apparent_bytes = 0u64;
    let game_name = capture
        .layout
        .game_dir
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("game"));
    let game_destination = capture.staging.join(game_name);
    let game_report = clone_snapshot_tree(&capture.layout.game_dir, &game_destination, cancel)?;
    absorb_report(&mut method, &mut apparent_bytes, &game_report);
    check_canceled(cancel)?;

    // The put-aside copies travel with the recipe whose records they keep
    // restorable.
    for (source, relative) in [
        (
            ephervex_storage::metadata::source_dir(&capture.layout.root),
            ".launcher/source",
        ),
        (
            ephervex_storage::metadata::put_aside_dir(&capture.layout.root),
            ".launcher/put-aside",
        ),
    ] {
        if !source.is_dir() {
            continue;
        }
        let destination = capture.staging.join(relative);
        std::fs::create_dir_all(destination.parent().unwrap_or(capture.staging)).map_err(
            |source| SnapshotError::Unwritable {
                path: destination.clone(),
                source,
            },
        )?;
        let report = clone_snapshot_tree(&source, &destination, cancel)?;
        absorb_report(&mut method, &mut apparent_bytes, &report);
        check_canceled(cancel)?;
    }
    check_canceled(cancel)?;

    let metadata_name = match capture.host {
        InstalledHost::Instance(_) => ephervex_storage::metadata::CLIENT_METADATA_FILE,
        InstalledHost::Server(_) => ephervex_storage::metadata::SERVER_METADATA_FILE,
    };
    let source_metadata =
        ephervex_storage::metadata::metadata_dir(&capture.layout.root).join(metadata_name);
    let destination_metadata = capture.staging.join(".launcher").join(metadata_name);
    std::fs::create_dir_all(destination_metadata.parent().unwrap_or(capture.staging)).map_err(
        |source| SnapshotError::Unwritable {
            path: destination_metadata.clone(),
            source,
        },
    )?;
    if clone_file(&source_metadata, &destination_metadata).map_err(|source| {
        SnapshotError::Unwritable {
            path: destination_metadata,
            source,
        }
    })? == CloneMethod::Copy
    {
        method = CloneMethod::Copy;
    }
    apparent_bytes = apparent_bytes.saturating_add(
        std::fs::metadata(&source_metadata)
            .map_err(|source| SnapshotError::Unreadable {
                path: source_metadata,
                source,
            })?
            .len(),
    );
    Ok((method, apparent_bytes))
}

fn clone_snapshot_tree(
    source: &Path,
    destination: &Path,
    cancel: &CancellationToken,
) -> Result<ephervex_storage::CloneReport, SnapshotError> {
    ephervex_storage::clone_tree_with_check(source, destination, &[], &mut || {
        if cancel.is_cancelled() {
            Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
        } else {
            Ok(())
        }
    })
    .map_err(|source| {
        if source.kind() == std::io::ErrorKind::Interrupted && cancel.is_cancelled() {
            SnapshotError::Canceled
        } else {
            SnapshotError::Unwritable {
                path: destination.to_path_buf(),
                source,
            }
        }
    })
}

fn restore_parts(
    host: InstalledHost,
    root: &Path,
    snapshot: &Path,
) -> Result<Vec<ephervex_storage::RestorePartState>, SnapshotError> {
    use ephervex_storage::{RestorePart, RestorePartState};
    let parts = match host {
        InstalledHost::Instance(_) => [
            RestorePart::ClientGame,
            RestorePart::Source,
            RestorePart::ClientMetadata,
            RestorePart::PutAside,
        ],
        InstalledHost::Server(_) => [
            RestorePart::ServerGame,
            RestorePart::Source,
            RestorePart::ServerMetadata,
            RestorePart::PutAside,
        ],
    };
    let states = parts
        .into_iter()
        .map(|part| RestorePartState {
            part,
            original_present: part_path(root, part).is_some(),
            replacement_present: part_path(snapshot, part).is_some(),
        })
        .collect::<Vec<_>>();
    if states[0].replacement_present && states[2].replacement_present {
        Ok(states)
    } else {
        Err(SnapshotError::Unreadable {
            path: snapshot.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "snapshot is missing its game directory or host metadata",
            ),
        })
    }
}

fn stage_restore_parts(
    transaction: &ephervex_storage::RestoreTransaction,
    snapshot: &Path,
    parts: &[ephervex_storage::RestorePartState],
    cancel: &CancellationToken,
) -> Result<(), SnapshotError> {
    for state in parts.iter().filter(|state| state.replacement_present) {
        check_canceled(cancel)?;
        let source = part_path(snapshot, state.part).ok_or_else(|| SnapshotError::Unreadable {
            path: snapshot.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "snapshot part vanished"),
        })?;
        let destination = transaction.incoming_path(state.part);
        if source.is_dir() {
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|source| SnapshotError::Unwritable {
                    path: destination.clone(),
                    source,
                })?;
            }
            clone_snapshot_tree(&source, &destination, cancel)?;
        } else {
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|source| SnapshotError::Unwritable {
                    path: destination.clone(),
                    source,
                })?;
            }
            clone_file(&source, &destination).map_err(|source| SnapshotError::Unwritable {
                path: destination,
                source,
            })?;
        }
    }
    check_canceled(cancel)
}

fn part_path(root: &Path, part: ephervex_storage::RestorePart) -> Option<PathBuf> {
    use ephervex_storage::RestorePart;
    let path = match part {
        RestorePart::ClientGame => root.join("minecraft"),
        RestorePart::ServerGame => root.join("server"),
        RestorePart::Source => root.join(".launcher/source"),
        RestorePart::ClientMetadata => root.join(".launcher/instance.json"),
        RestorePart::ServerMetadata => root.join(".launcher/server.json"),
        RestorePart::PutAside => root.join(".launcher/put-aside"),
    };
    std::fs::symlink_metadata(&path).ok().map(|_| path)
}

fn snapshot_label(label: &str) -> Result<String, SnapshotError> {
    let label = label.trim();
    if label.is_empty()
        || label.chars().count() > MAX_SNAPSHOT_LABEL_CHARS
        || label.chars().any(char::is_control)
    {
        Err(SnapshotError::LabelInvalid)
    } else {
        Ok(label.to_owned())
    }
}

fn check_canceled(cancel: &CancellationToken) -> Result<(), SnapshotError> {
    if cancel.is_cancelled() {
        Err(SnapshotError::Canceled)
    } else {
        Ok(())
    }
}

fn absorb_report(
    method: &mut CloneMethod,
    bytes: &mut u64,
    report: &ephervex_storage::CloneReport,
) {
    if report.method == CloneMethod::Copy {
        *method = CloneMethod::Copy;
    }
    *bytes = bytes.saturating_add(report.apparent_bytes);
}

fn write_manifest(directory: &Path, record: &SnapshotRecord) -> Result<(), SnapshotError> {
    let path = directory.join(SNAPSHOT_MANIFEST);
    ephervex_storage::write_atomic(
        &path,
        ephervex_storage::AtomicWriteOptions::private(),
        |file| {
            serde_json::to_writer(file, &SnapshotDocument::new(record.clone()))
                .map_err(std::io::Error::other)
        },
    )
    .map_err(|source| SnapshotError::Unwritable { path, source })
}

fn read_manifest(directory: &Path) -> Result<SnapshotRecord, SnapshotError> {
    let path = directory.join(SNAPSHOT_MANIFEST);
    let bytes = std::fs::read(&path).map_err(|source| SnapshotError::Unreadable {
        path: path.clone(),
        source,
    })?;
    let document: SnapshotDocument =
        serde_json::from_slice(&bytes).map_err(|source| SnapshotError::Invalid {
            path: path.clone(),
            source,
        })?;
    if document.snapshot_version > SNAPSHOT_VERSION {
        return Err(SnapshotError::UnsupportedVersion {
            path,
            found: document.snapshot_version,
            supported: SNAPSHOT_VERSION,
        });
    }
    Ok(document.record)
}

/// Who is asking for a restore: it decides whether an isolation session
/// refuses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotRestoreOwner {
    Caller,
    IsolationSession,
}

fn snapshot_core_error(error: crate::CoreError) -> SnapshotError {
    if error.is_cancellation() {
        return SnapshotError::Canceled;
    }
    let path = error
        .failing_path()
        .unwrap_or_else(|| PathBuf::from("snapshot host"));
    SnapshotError::Unreadable {
        path,
        source: std::io::Error::other(error),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use ephervex_model::{LoaderKind, LoaderSelection};
    use ephervex_storage::{DuplicateClient, NewClient, NewServer, Paths};

    use super::*;

    #[test]
    fn a_snapshot_records_the_committed_target_while_the_index_row_lags() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "snapshot",
                "1.21.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "21.1.100"),
            ))
            .expect("client");
        let committed = app
            .commit_client_without_index_for_test(client.id, |committed| {
                committed.loader = LoaderSelection::exact(LoaderKind::NeoForge, "21.1.172");
            })
            .expect("commit");

        let snapshot = app
            .create_snapshot(
                InstalledHost::Instance(client.id),
                "lagging",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("snapshot");

        assert_eq!(snapshot.loader, committed.loader);
    }

    #[test]
    fn failed_capture_cleans_staging_and_leaves_live_state_untouched() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "snapshot",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("client");
        let host = InstalledHost::Instance(client.id);
        let game = ephervex_storage::metadata::client_game_dir(&client.root);
        // A named pipe is a special file capture cannot clone. Unlike a socket
        // it has no path-length limit, so the test does not depend on TMPDIR.
        rustix::fs::mknodat(
            rustix::fs::CWD,
            game.join("pipe"),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            0,
        )
        .expect("named pipe");
        assert!(
            app.create_snapshot(
                host,
                "fails",
                SnapshotOrigin::Manual,
                &CancellationToken::new()
            )
            .is_err()
        );
        assert!(game.join("pipe").exists());
        assert_eq!(
            std::fs::read_dir(ephervex_storage::metadata::staging_dir(&client.root))
                .expect("staging")
                .count(),
            0
        );
        assert!(app.snapshots(host).expect("snapshots").is_empty());
    }

    #[test]
    fn canceled_capture_does_not_create_staging_or_snapshot_files() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "snapshot",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("client");
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            app.create_snapshot(
                InstalledHost::Instance(client.id),
                "canceled",
                SnapshotOrigin::Manual,
                &cancel
            ),
            Err(SnapshotError::Canceled)
        ));
        assert!(!ephervex_storage::metadata::staging_dir(&client.root).exists());
    }

    #[test]
    fn a_client_snapshot_captures_disabled_jars_and_excludes_transaction_state() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "snapshot client",
                "1.21.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "21.1.1"),
            ))
            .expect("create client");
        let game = ephervex_storage::metadata::client_game_dir(&client.root);
        std::fs::create_dir_all(game.join("mods")).expect("create mods");
        std::fs::write(game.join("mods/test.jar.disabled"), b"disabled").expect("write jar");
        std::fs::create_dir_all(ephervex_storage::metadata::retired_dir(&client.root))
            .expect("create retired");
        std::fs::write(
            ephervex_storage::metadata::retired_dir(&client.root).join("old.jar"),
            b"old",
        )
        .expect("write retired");

        let record = app
            .create_snapshot(
                InstalledHost::Instance(client.id),
                "before debugging",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");
        let snapshot =
            ephervex_storage::metadata::snapshots_dir(&client.root).join(record.id.to_string());

        assert_eq!(
            std::fs::read(snapshot.join("minecraft/mods/test.jar.disabled")).expect("read jar"),
            b"disabled"
        );
        assert!(!snapshot.join(".launcher/retired").exists());
        assert!(!snapshot.join(".launcher/staging").exists());
        assert!(!snapshot.join(".launcher/snapshots").exists());
    }

    #[test]
    fn a_server_snapshot_keeps_the_shared_library_link_as_a_link() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let server = app
            .registry_for_services()
            .create_server(NewServer::new(
                "snapshot server",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create server");
        let game = ephervex_storage::metadata::server_game_dir(&server.root);
        let shared = temporary.path().join("shared-libraries");
        std::fs::create_dir_all(&shared).expect("create shared");
        std::os::unix::fs::symlink(&shared, game.join("libraries")).expect("link libraries");

        let record = app
            .create_snapshot(
                InstalledHost::Server(server.id),
                "server state",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");
        let link = ephervex_storage::metadata::snapshots_dir(&server.root)
            .join(record.id.to_string())
            .join("server/libraries");

        assert!(
            std::fs::symlink_metadata(&link)
                .expect("metadata")
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_link(link).expect("read link"), shared);
    }

    #[test]
    fn snapshot_labels_can_change_without_changing_the_directory_name() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "snapshot client",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let created = app
            .create_snapshot(
                InstalledHost::Instance(client.id),
                "first label",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");

        let renamed = app
            .rename_snapshot(
                InstalledHost::Instance(client.id),
                created.id,
                "second label",
            )
            .expect("rename snapshot");

        assert_eq!(renamed.label, "second label");
        assert_eq!(
            app.snapshots(InstalledHost::Instance(client.id))
                .expect("list")[0],
            renamed
        );
    }

    #[test]
    fn restoring_a_snapshot_replaces_live_state_and_keeps_a_safety_snapshot() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "restore client",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let game = ephervex_storage::metadata::client_game_dir(&client.root);
        let source = ephervex_storage::metadata::source_dir(&client.root);
        std::fs::write(game.join("state.txt"), b"captured").expect("write captured state");
        std::fs::create_dir_all(&source).expect("create source");
        std::fs::write(source.join("note.txt"), b"captured source").expect("write captured source");
        let captured = app
            .create_snapshot(
                InstalledHost::Instance(client.id),
                "known good",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");

        std::fs::write(game.join("state.txt"), b"changed").expect("change state");
        std::fs::write(game.join("new.txt"), b"must disappear").expect("write new state");
        std::fs::write(source.join("note.txt"), b"changed source").expect("change source");
        let restored = app
            .restore_snapshot(
                InstalledHost::Instance(client.id),
                captured.id,
                SnapshotRestoreOptions::default(),
                &CancellationToken::new(),
            )
            .expect("restore snapshot");

        assert_eq!(restored.restored, captured);
        assert_eq!(
            std::fs::read(game.join("state.txt")).expect("read"),
            b"captured"
        );
        assert!(
            !game.join("new.txt").exists(),
            "restore is replacement, not merge"
        );
        assert_eq!(
            std::fs::read(source.join("note.txt")).expect("read source"),
            b"captured source"
        );
        let safety = restored.safety_snapshot.expect("safety snapshot");
        assert_eq!(safety.origin, SnapshotOrigin::BeforeRestore);
        assert_eq!(
            std::fs::read(
                ephervex_storage::metadata::snapshots_dir(&client.root)
                    .join(safety.id.to_string())
                    .join("minecraft/state.txt")
            )
            .expect("read safety state"),
            b"changed"
        );
    }

    #[test]
    fn restoring_a_snapshot_keeps_the_issue_link_the_instance_has_now() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(
                NewClient::new("link restore", "1.21.1", LoaderSelection::vanilla())
                    .with_issue_link("https://example.invalid/issues/1"),
            )
            .expect("create client");
        let host = InstalledHost::Instance(client.id);
        let captured = app
            .create_snapshot(
                host,
                "old link",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");

        app.set_client_issue_link(client.id, Some("https://example.invalid/issues/2"))
            .expect("change the link");
        app.restore_snapshot(
            host,
            captured.id,
            SnapshotRestoreOptions::default(),
            &CancellationToken::new(),
        )
        .expect("restore snapshot");

        assert_eq!(
            app.current_client(client.id)
                .expect("client")
                .issue_link
                .as_deref(),
            Some("https://example.invalid/issues/2")
        );

        app.set_client_issue_link(client.id, None)
            .expect("clear the link");
        app.restore_snapshot(
            host,
            captured.id,
            SnapshotRestoreOptions::default(),
            &CancellationToken::new(),
        )
        .expect("restore snapshot again");
        assert_eq!(
            app.current_client(client.id).expect("client").issue_link,
            None,
            "a cleared link is not brought back by the snapshot"
        );
    }

    #[test]
    fn restoring_over_unreadable_metadata_keeps_the_snapshots_issue_link() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(
                NewClient::new("damaged", "1.21.1", LoaderSelection::vanilla())
                    .with_issue_link("https://example.invalid/issues/1"),
            )
            .expect("create client");
        let host = InstalledHost::Instance(client.id);
        let captured = app
            .create_snapshot(
                host,
                "with link",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");
        std::fs::write(
            ephervex_storage::metadata::client_metadata_path(&client.root),
            b"{ not json",
        )
        .expect("damage metadata");

        app.restore_snapshot(
            host,
            captured.id,
            SnapshotRestoreOptions {
                safety_snapshot: false,
            },
            &CancellationToken::new(),
        )
        .expect("restore snapshot");

        assert_eq!(
            app.current_client(client.id)
                .expect("client")
                .issue_link
                .as_deref(),
            Some("https://example.invalid/issues/1")
        );
    }

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("create directory");
        for entry in std::fs::read_dir(from).expect("read directory") {
            let entry = entry.expect("entry");
            let target = to.join(entry.file_name());
            if entry.file_type().expect("file type").is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).expect("copy file");
            }
        }
    }

    /// The state a copy-mode promotion leaves when its index write and its
    /// source removal both fail: the row still names the temporary root, the
    /// stale temporary tree stands, and a newer copy marked persistent stands
    /// at the persistent root.
    fn commit_persistent_copy_beside_the_temporary_tree(
        app: &App,
        client: &ephervex_storage::ClientRecord,
    ) -> PathBuf {
        let persistent = app
            .paths()
            .instance_root(ephervex_model::StorageClass::Persistent, client.id);
        std::fs::create_dir_all(persistent.parent().expect("instances directory"))
            .expect("create instances directory");
        copy_tree(&client.root, &persistent);
        let mut promoted =
            ephervex_storage::metadata::read_client_metadata(&persistent).expect("copied metadata");
        promoted.root.clone_from(&persistent);
        promoted.storage = ephervex_model::StorageClass::Persistent;
        promoted.updated_at = client.updated_at + chrono::Duration::seconds(1);
        ephervex_storage::metadata::write_client_metadata(&persistent, &promoted)
            .expect("promoted metadata");
        assert_eq!(app.client(client.id).expect("followed").root, persistent);
        assert_eq!(
            app.registry().indexed_client(client.id).expect("row").root,
            client.root
        );
        persistent
    }

    fn restore_with_defaults(app: &App, client: InstalledHost, snapshot: SnapshotId) {
        app.restore_snapshot(
            client,
            snapshot,
            SnapshotRestoreOptions::default(),
            &CancellationToken::new(),
        )
        .expect("restore snapshot");
    }

    #[test]
    fn restoring_a_client_whose_row_names_a_superseded_temporary_copy_repairs_the_row_and_removes_the_copy()
     {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "restore client",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let host = InstalledHost::Instance(client.id);
        let game = ephervex_storage::metadata::client_game_dir(&client.root);
        std::fs::write(game.join("state.txt"), b"captured").expect("write captured state");
        let captured = app
            .create_snapshot(
                host,
                "known good",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");
        let persistent = commit_persistent_copy_beside_the_temporary_tree(&app, &client);
        let persistent_game = ephervex_storage::metadata::client_game_dir(&persistent);
        std::fs::write(persistent_game.join("state.txt"), b"changed").expect("change state");
        restore_with_defaults(&app, host, captured.id);

        let row = app.registry().indexed_client(client.id).expect("row");
        assert_eq!(row.root, persistent);
        assert_eq!(row.storage, ephervex_model::StorageClass::Persistent);
        assert_eq!(app.client(client.id).expect("client").root, persistent);
        assert_eq!(
            app.current_client(client.id).expect("current").root,
            persistent
        );
        assert_eq!(
            std::fs::read(persistent_game.join("state.txt")).expect("restored state"),
            b"captured"
        );
        assert_eq!(
            ephervex_storage::metadata::read_client_metadata(&persistent)
                .expect("restored metadata")
                .root,
            persistent
        );
        assert!(
            !client.root.exists(),
            "the superseded temporary tree is removed once the restore is final"
        );
    }

    #[test]
    fn restoring_a_client_whose_row_names_a_vacated_temporary_root_repairs_the_row() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "restore client",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let host = InstalledHost::Instance(client.id);
        let game = ephervex_storage::metadata::client_game_dir(&client.root);
        std::fs::write(game.join("state.txt"), b"captured").expect("write captured state");
        let captured = app
            .create_snapshot(
                host,
                "known good",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");
        let persistent = commit_persistent_copy_beside_the_temporary_tree(&app, &client);
        std::fs::remove_dir_all(&client.root).expect("vacate the temporary root");

        restore_with_defaults(&app, host, captured.id);

        let row = app.registry().indexed_client(client.id).expect("row");
        assert_eq!(row.root, persistent);
        assert_eq!(row.storage, ephervex_model::StorageClass::Persistent);
        assert_eq!(app.client(client.id).expect("client").root, persistent);
        assert!(
            !client.root.exists(),
            "the vacated temporary root is not recreated"
        );
    }

    #[test]
    fn restoring_a_server_whose_row_names_a_superseded_temporary_copy_records_the_persistent_root_and_removes_the_copy()
     {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let server = app
            .registry_for_services()
            .create_server(NewServer::new(
                "Repro Server",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create server");
        let host = InstalledHost::Server(server.id);
        let game = ephervex_storage::metadata::server_game_dir(&server.root);
        std::fs::write(game.join("state.txt"), b"captured").expect("write captured state");
        let captured = app
            .create_snapshot(
                host,
                "known good",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");
        let persistent = app
            .paths()
            .server_root(ephervex_model::StorageClass::Persistent, server.id);
        std::fs::create_dir_all(persistent.parent().expect("servers directory"))
            .expect("create servers directory");
        copy_tree(&server.root, &persistent);
        let mut promoted =
            ephervex_storage::metadata::read_server_metadata(&persistent).expect("copied metadata");
        promoted.root.clone_from(&persistent);
        promoted.storage = ephervex_model::StorageClass::Persistent;
        promoted.updated_at = server.updated_at + chrono::Duration::seconds(1);
        ephervex_storage::metadata::write_server_metadata(&persistent, &promoted)
            .expect("promoted metadata");
        let persistent_game = ephervex_storage::metadata::server_game_dir(&persistent);
        std::fs::write(persistent_game.join("state.txt"), b"changed").expect("change state");
        assert_eq!(app.server(server.id).expect("followed").root, persistent);

        let restore = app
            .restore_snapshot(
                host,
                captured.id,
                SnapshotRestoreOptions::default(),
                &CancellationToken::new(),
            )
            .expect("restore snapshot");

        assert!(restore.server_warnings.is_empty());
        let row = app
            .registry_for_services()
            .indexed_server(server.id)
            .expect("row");
        assert_eq!(row.root, persistent);
        assert_eq!(row.storage, ephervex_model::StorageClass::Persistent);
        assert_eq!(app.server(server.id).expect("server").root, persistent);
        assert_eq!(
            std::fs::read(persistent_game.join("state.txt")).expect("restored state"),
            b"captured"
        );
        assert!(
            !server.root.exists(),
            "the superseded temporary tree is removed once the restore is final"
        );
    }

    fn freeze_directory(game: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let frozen = game.join("frozen");
        std::fs::create_dir_all(&frozen).expect("create frozen directory");
        std::fs::write(frozen.join("kept.txt"), b"frozen").expect("write frozen file");
        std::fs::set_permissions(&frozen, std::fs::Permissions::from_mode(0o555))
            .expect("make the directory read-only");
        frozen
    }

    #[test]
    fn a_snapshot_of_a_host_with_a_read_only_directory_can_be_deleted() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "frozen client",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let host = InstalledHost::Instance(client.id);
        let game = ephervex_storage::metadata::client_game_dir(&client.root);
        freeze_directory(&game);
        let captured = app
            .create_snapshot(
                host,
                "frozen",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");

        let deleted = app.delete_snapshot(host, captured.id);

        let remaining = app.snapshots(host).expect("list").len();
        let leftover = ephervex_storage::metadata::snapshots_dir(&client.root)
            .join(captured.id.to_string())
            .exists();
        ephervex_storage::remove_tree_if_present(temporary.path()).expect("clean up");
        deleted.expect("a read-only directory does not block deletion");
        assert_eq!(remaining, 0);
        assert!(!leftover);
    }

    #[test]
    fn restoring_over_a_host_with_a_read_only_directory_completes_and_leaves_no_staging() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "frozen restore",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let host = InstalledHost::Instance(client.id);
        let game = ephervex_storage::metadata::client_game_dir(&client.root);
        std::fs::write(game.join("state.txt"), b"captured").expect("write captured state");
        let captured = app
            .create_snapshot(
                host,
                "plain",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");
        freeze_directory(&game);
        std::fs::write(game.join("state.txt"), b"changed").expect("change state");

        let first = app.restore_snapshot(
            host,
            captured.id,
            SnapshotRestoreOptions::default(),
            &CancellationToken::new(),
        );
        let state = std::fs::read(game.join("state.txt")).ok();
        let frozen_left = game.join("frozen").exists();
        let staging = std::fs::read_dir(ephervex_storage::metadata::staging_dir(&client.root))
            .map_or(0, Iterator::count);
        let second = app.restore_snapshot(
            host,
            captured.id,
            SnapshotRestoreOptions::default(),
            &CancellationToken::new(),
        );
        ephervex_storage::remove_tree_if_present(temporary.path()).expect("clean up");

        first.expect("the restore succeeds over a read-only directory");
        assert_eq!(state.as_deref(), Some(b"captured".as_slice()));
        assert!(!frozen_left, "restore is replacement, not merge");
        assert_eq!(staging, 0, "the finished restore leaves no staging behind");
        second.expect("a later restore is not blocked by leftover staging");
    }

    #[test]
    fn a_host_held_by_another_operation_refuses_a_snapshot_capture() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "busy client",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let operation = app
            .registry()
            .claim_host(ephervex_storage::RecordRef::Client(client.id))
            .expect("claim");

        let error = app
            .create_snapshot(
                InstalledHost::Instance(client.id),
                "during a change",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect_err("another operation holds the host");

        assert!(matches!(error, SnapshotError::HostBusy { .. }), "{error}");
        drop(operation);
        assert!(
            app.snapshots(InstalledHost::Instance(client.id))
                .expect("list snapshots")
                .is_empty(),
            "a refused capture leaves no snapshot"
        );
    }

    #[test]
    fn a_host_held_by_a_move_refuses_snapshot_restoration_untouched() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "moving client",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let game = ephervex_storage::metadata::client_game_dir(&client.root);
        std::fs::write(game.join("state.txt"), b"captured").expect("write state");
        let snapshot = app
            .create_snapshot(
                InstalledHost::Instance(client.id),
                "known good",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");
        std::fs::write(game.join("state.txt"), b"changed").expect("change state");
        let _move = app
            .registry()
            .claim_host(ephervex_storage::RecordRef::Client(client.id))
            .expect("claim");

        let error = app
            .restore_snapshot(
                InstalledHost::Instance(client.id),
                snapshot.id,
                SnapshotRestoreOptions::default(),
                &CancellationToken::new(),
            )
            .expect_err("a move holds the host");

        assert!(matches!(error, SnapshotError::HostBusy { .. }));
        assert_eq!(error.message_id(), "error-snapshot-host-busy");
        assert_eq!(
            std::fs::read(game.join("state.txt")).expect("read"),
            b"changed"
        );
        assert_eq!(
            app.snapshots(InstalledHost::Instance(client.id))
                .expect("list snapshots")
                .len(),
            1,
            "refusal must happen before creating a safety snapshot"
        );
    }

    #[test]
    fn a_restore_canceled_while_another_launcher_changes_the_recipe_is_reported_as_canceled() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "busy client",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let host = InstalledHost::Instance(client.id);
        let snapshot = app
            .create_snapshot(
                host,
                "known good",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");
        let other_launcher =
            ephervex_storage::Registry::open_in_memory(Paths::rooted_at(temporary.path()))
                .expect("registry");
        let _recipe = other_launcher
            .try_claim_recipe(host.record_ref())
            .expect("try")
            .expect("free");
        let cancel = CancellationToken::new();
        let canceler = std::thread::spawn({
            let cancel = cancel.clone();
            move || {
                std::thread::sleep(std::time::Duration::from_millis(150));
                cancel.cancel();
            }
        });

        let error = app
            .restore_snapshot(
                host,
                snapshot.id,
                SnapshotRestoreOptions::default(),
                &cancel,
            )
            .expect_err("the recipe is being changed elsewhere");

        canceler.join().expect("canceler finishes");
        assert!(error.is_cancellation(), "{error:?}");
    }

    #[test]
    fn a_running_client_refuses_snapshot_restoration() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "running client",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let snapshot = app
            .create_snapshot(
                InstalledHost::Instance(client.id),
                "before launch",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");
        let _lock = ephervex_storage::LaunchLock::acquire(&client.root).expect("launch lock");

        let error = app
            .restore_snapshot(
                InstalledHost::Instance(client.id),
                snapshot.id,
                SnapshotRestoreOptions::default(),
                &CancellationToken::new(),
            )
            .expect_err("running host must be refused");

        assert!(matches!(error, SnapshotError::HostRunning { .. }));
        assert_eq!(
            app.snapshots(InstalledHost::Instance(client.id))
                .expect("list snapshots")
                .len(),
            1,
            "refusal must happen before creating a safety snapshot"
        );
    }

    #[test]
    fn a_fresh_app_reconciles_an_interrupted_restore_to_the_original_state() {
        use ephervex_storage::{RestorePart, RestorePartState};

        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = Paths::rooted_at(temporary.path());
        let app = App::open_at(paths.clone()).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "interrupted restore",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let game = ephervex_storage::metadata::client_game_dir(&client.root);
        std::fs::write(game.join("state.txt"), b"before").expect("write original");
        let transaction = ephervex_storage::prepare_restore(
            &client.root,
            ephervex_model::SnapshotId::new(),
            vec![RestorePartState {
                part: RestorePart::ClientGame,
                original_present: true,
                replacement_present: true,
            }],
        )
        .expect("prepare restore");
        let incoming = transaction.incoming_path(RestorePart::ClientGame);
        std::fs::create_dir_all(&incoming).expect("create incoming");
        std::fs::write(incoming.join("state.txt"), b"snapshot").expect("write snapshot state");
        ephervex_storage::metadata::mark_incomplete(&client.root).expect("mark incomplete");
        let replaced = transaction.directory().join("replaced/minecraft");
        std::fs::create_dir_all(replaced.parent().expect("replaced parent"))
            .expect("create replaced parent");
        std::fs::rename(&game, &replaced).expect("retire live state");
        std::fs::rename(&incoming, &game).expect("publish snapshot state");
        drop(app);

        let reopened = App::open_at(paths).expect("reopen app");
        let report = reopened.reconcile().expect("reconcile");

        assert_eq!(report.restores_rolled_back.len(), 1);
        assert_eq!(
            std::fs::read(game.join("state.txt")).expect("read recovered state"),
            b"before"
        );
        assert!(!transaction.directory().exists());
        assert!(!ephervex_storage::metadata::is_incomplete(&client.root));
    }

    fn listed_servers(client_root: &Path) -> Vec<(String, String)> {
        let path = ephervex_storage::metadata::client_game_dir(client_root).join("servers.dat");
        let bytes = std::fs::read(path).expect("read servers.dat");
        let root: std::collections::HashMap<String, fastnbt::Value> =
            fastnbt::from_bytes(&bytes).expect("valid nbt");
        let Some(fastnbt::Value::List(list)) = root.get("servers") else {
            return Vec::new();
        };
        let text =
            |fields: &std::collections::HashMap<String, fastnbt::Value>, key: &str| match fields
                .get(key)
            {
                Some(fastnbt::Value::String(value)) => Some(value.clone()),
                _ => None,
            };
        list.iter()
            .filter_map(|item| match item {
                fastnbt::Value::Compound(fields) => {
                    Some((text(fields, "name")?, text(fields, "ip")?))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn restoring_a_server_snapshot_reports_an_index_failure_after_the_metadata_is_restored() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let server = app
            .registry_for_services()
            .create_server(ephervex_storage::NewServer::new(
                "Repro Server",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create server");
        let snapshot = app
            .create_snapshot(
                InstalledHost::Server(server.id),
                "before",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");
        app.registry_for_services()
            .index()
            .connection()
            .execute_batch(
                "CREATE TRIGGER refuse_server_updates BEFORE UPDATE ON servers
                 BEGIN SELECT RAISE(ABORT, 'index unavailable'); END;",
            )
            .expect("install failure");

        let restore = app
            .restore_snapshot(
                InstalledHost::Server(server.id),
                snapshot.id,
                SnapshotRestoreOptions::default(),
                &CancellationToken::new(),
            )
            .expect("the restored metadata is a committed restore");

        assert!(matches!(
            restore.server_warnings.as_slice(),
            [ephervex_storage::ServerCommitWarning::IndexOutOfSync { .. }]
        ));
        assert!(restore.warnings.is_empty());
    }

    #[test]
    fn restoring_a_server_snapshot_puts_its_address_back_in_the_linked_clients_list() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "linked client",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let server = app
            .registry_for_services()
            .create_server(ephervex_storage::NewServer::linked(
                "Repro Server",
                client.id,
            ))
            .expect("create server");
        app.sync_linked_server_list(&server, None);
        let snapshot = app
            .create_snapshot(
                InstalledHost::Server(server.id),
                "before port change",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");
        app.update_server_network(
            server.id,
            &crate::ServerNetworkChange {
                port: Some(25_571),
                ..Default::default()
            },
        )
        .expect("port change");
        assert_eq!(
            listed_servers(&client.root),
            vec![("Repro Server".to_owned(), "127.0.0.1:25571".to_owned())]
        );

        app.restore_snapshot(
            InstalledHost::Server(server.id),
            snapshot.id,
            SnapshotRestoreOptions::default(),
            &CancellationToken::new(),
        )
        .expect("restore");

        assert_eq!(
            listed_servers(&client.root),
            vec![("Repro Server".to_owned(), "127.0.0.1:25565".to_owned())]
        );
    }

    #[test]
    fn restoring_a_server_snapshot_forgets_properties_kept_for_its_next_start() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let server = app
            .registry_for_services()
            .create_server(ephervex_storage::NewServer::new(
                "Repro Server",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create server");
        app.set_server_properties(
            server.id,
            server.network.clone(),
            &[("motd".to_owned(), "captured".to_owned())],
        )
        .expect("stopped save");
        let snapshot = app
            .create_snapshot(
                InstalledHost::Server(server.id),
                "before a running save",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");
        // What a save while the server ran leaves behind once it was stopped rather than restarted.
        ephervex_server::save_while_running(
            &server.root,
            &ephervex_storage::metadata::server_game_dir(&server.root)
                .join(ephervex_server::SERVER_PROPERTIES_FILE),
            &[("motd".to_owned(), "saved while running".to_owned())],
        )
        .expect("running save");

        app.restore_snapshot(
            InstalledHost::Server(server.id),
            snapshot.id,
            SnapshotRestoreOptions::default(),
            &CancellationToken::new(),
        )
        .expect("restore");

        assert!(
            ephervex_server::PendingProperties::read(&server.root)
                .expect("kept")
                .is_empty()
        );
        assert_eq!(
            app.server_properties_at_next_start(server.id)
                .expect("properties")
                .get("motd")
                .as_deref(),
            Some("captured")
        );
    }

    #[test]
    fn restoring_a_client_snapshot_lists_every_server_linked_since_it_was_taken() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "linked client",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let early = app
            .registry_for_services()
            .create_server(ephervex_storage::NewServer::linked(
                "Early Server",
                client.id,
            ))
            .expect("create server");
        app.sync_linked_server_list(&early, None);
        let snapshot = app
            .create_snapshot(
                InstalledHost::Instance(client.id),
                "before second server",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect("create snapshot");
        let mut late_new = ephervex_storage::NewServer::linked("Late Server", client.id);
        late_new.network.port = 25_580;
        let late = app
            .registry_for_services()
            .create_server(late_new)
            .expect("create server");
        app.sync_linked_server_list(&late, None);
        assert_eq!(listed_servers(&client.root).len(), 2);

        app.restore_snapshot(
            InstalledHost::Instance(client.id),
            snapshot.id,
            SnapshotRestoreOptions::default(),
            &CancellationToken::new(),
        )
        .expect("restore");

        let mut listed = listed_servers(&client.root);
        listed.sort();
        assert_eq!(
            listed,
            vec![
                ("Early Server".to_owned(), "127.0.0.1:25565".to_owned()),
                ("Late Server".to_owned(), "127.0.0.1:25580".to_owned()),
            ]
        );
    }

    #[test]
    fn a_running_client_refuses_snapshot_creation() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "running client",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let _lock = ephervex_storage::LaunchLock::acquire(&client.root).expect("launch lock");

        let error = app
            .create_snapshot(
                InstalledHost::Instance(client.id),
                "unsafe capture",
                SnapshotOrigin::Manual,
                &CancellationToken::new(),
            )
            .expect_err("running host must be refused");

        assert!(matches!(error, SnapshotError::HostRunning { .. }));
        assert!(
            ephervex_storage::metadata::snapshots_dir(&client.root)
                .read_dir()
                .is_err(),
            "refusal happens before snapshot storage is created"
        );
    }

    #[test]
    fn duplicating_a_client_does_not_copy_its_snapshot_history() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app = App::open_at(Paths::rooted_at(temporary.path())).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "snapshotted client",
                "1.21.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        app.create_snapshot(
            InstalledHost::Instance(client.id),
            "history",
            SnapshotOrigin::Manual,
            &CancellationToken::new(),
        )
        .expect("create snapshot");

        let duplicate = app
            .registry_for_services()
            .duplicate_client(client.id, &DuplicateClient::default())
            .expect("duplicate client");

        assert!(
            !ephervex_storage::metadata::snapshots_dir(&duplicate.record.root).exists(),
            "snapshot history is not recursively duplicated"
        );
    }
}
