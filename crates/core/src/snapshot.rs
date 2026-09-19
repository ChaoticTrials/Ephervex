//! Local, user-managed safety snapshots of installed clients and servers.
//!
//! A snapshot captures runnable reproduction state, not an export archive. It
//! lives under the host root, never contains snapshot history, and preserves
//! symlinks instead of following them into shared stores.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ephervex_model::{LoaderSelection, SnapshotId};
use ephervex_storage::{CloneMethod, clone_file};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio_util::sync::CancellationToken;

use crate::{App, InstalledHost, ModPresence};

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
    #[error("snapshot {id} does not exist")]
    NotFound { id: SnapshotId },
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
            Self::NotFound { .. } => "error-snapshot-not-found",
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
            Self::Unreadable { path, .. }
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
    pub fn create_snapshot(
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
                std::fs::remove_dir_all(&staging).map_err(|source| SnapshotError::Unwritable {
                    path: staging.clone(),
                    source: std::io::Error::other(format!("{error}; cleanup failed: {source}")),
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

    /// Deletes one explicitly selected snapshot.
    pub fn delete_snapshot(
        &self,
        host: InstalledHost,
        id: SnapshotId,
    ) -> Result<(), SnapshotError> {
        let directory = self.snapshot_directory(host, id)?;
        read_manifest(&directory)?;
        std::fs::remove_dir_all(&directory).map_err(|source| SnapshotError::Unwritable {
            path: directory.clone(),
            source,
        })?;
        ephervex_storage::sync_parent(&directory).map_err(|source| SnapshotError::Unwritable {
            path: directory,
            source,
        })
    }

    fn capture_snapshot(
        &self,
        capture: SnapshotCapture<'_>,
        cancel: &CancellationToken,
    ) -> Result<SnapshotRecord, SnapshotError> {
        let (method, apparent_bytes) = capture_files(&capture, cancel)?;

        let (minecraft_version, loader) = match capture.host {
            InstalledHost::Instance(id) => {
                let record = self.client(id).map_err(snapshot_core_error)?;
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

    let source = ephervex_storage::metadata::source_dir(&capture.layout.root);
    if source.is_dir() {
        let destination = capture.staging.join(".launcher/source");
        std::fs::create_dir_all(destination.parent().unwrap_or(capture.staging)).map_err(
            |source| SnapshotError::Unwritable {
                path: destination.clone(),
                source,
            },
        )?;
        let report = clone_snapshot_tree(&source, &destination, cancel)?;
        absorb_report(&mut method, &mut apparent_bytes, &report);
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

fn snapshot_core_error(error: crate::CoreError) -> SnapshotError {
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
        let socket = std::os::unix::net::UnixListener::bind(game.join("socket")).expect("socket");
        assert!(
            app.create_snapshot(
                host,
                "fails",
                SnapshotOrigin::Manual,
                &CancellationToken::new()
            )
            .is_err()
        );
        assert!(game.join("socket").exists());
        assert_eq!(
            std::fs::read_dir(ephervex_storage::metadata::staging_dir(&client.root))
                .expect("staging")
                .count(),
            0
        );
        assert!(app.snapshots(host).expect("snapshots").is_empty());
        drop(socket);
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
