//! Per-instance launcher metadata (ADR 0002).
//!
//! ```text
//! instance-root/            server-root/
//! ├──.launcher/            ├──.launcher/
//! │   ├── instance.json     │   ├── server.json
//! │   ├── source/           │   ├── source/
//! │   └── warnings.json     │   └── warnings.json
//! └── minecraft/            └── server/
//! ```
//!
//! # Why this exists at all
//!
//! The SQLite index is a cache of authority, not the authority. A user who
//! copies an instance directory to another machine, or who loses
//! `launcher.sqlite` to a bad shutdown, still has everything needed to
//! reconstruct the record - because it is sitting in the directory they care
//! about.
//!
//! # Write order
//!
//! Metadata is written *before* the index row, always. A crash between the two
//! leaves a directory that startup reconciliation will re-index; the reverse
//! order would leave an index row pointing at nothing.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atomic::AtomicPublication;
use crate::paths::{LAUNCHER_METADATA_DIR, PathError, create_dir_all};
use crate::records::{ClientRecord, ServerRecord};

/// The metadata file format version.
///
/// Separate from the database schema version: an instance directory can be
/// carried to a machine running a different build, so the file has to describe
/// itself.
pub const METADATA_VERSION: u32 = 1;

/// `.launcher/instance.json`.
pub const CLIENT_METADATA_FILE: &str = "instance.json";
/// `.launcher/server.json`.
pub const SERVER_METADATA_FILE: &str = "server.json";
/// `.launcher/warnings.json`.
pub const WARNINGS_FILE: &str = "warnings.json";
/// `.launcher/incomplete`, present only while an instance is being built.
///
/// Materializing a client writes files for as long as the pack takes, and a
/// power loss in the middle of that leaves a root whose `instance.json` parses
/// perfectly and whose `mods/` is half a pack. The index cannot describe that
/// state -- an unpublished instance simply has no row -- but reconciliation
/// reads directories rather than the index, so it needs a signal of its own.
/// This file is that signal: present from creation until publication, and its
/// absence is what makes an orphan directory safe to adopt (ADR 0002).
pub const INCOMPLETE_MARKER_FILE: &str = "incomplete";
/// `.launcher/source/`, holding the provider-specific recipe.
pub const SOURCE_DIR: &str = "source";
/// `.launcher/retired/`, where a mutation holds the files it is replacing.
///
/// A version change writes the new jar, moves the old one here, and only then
/// publishes the recipe. If any of that fails the old jar is moved back, which
/// a delete could not offer. Inside `.launcher/` because it is launcher-owned
/// scratch the game must never see, and inside the *instance* because a rename
/// is only atomic within one filesystem.
pub const RETIRED_DIR: &str = "retired";
/// `.launcher/staging/`, where an incoming file waits until it is complete.
///
/// Kept apart from [`RETIRED_DIR`], which holds files on their way *out*. A
/// jar copied in from elsewhere lands here first and is renamed into `mods/`
/// once every byte is written, so an interrupted copy cannot leave a truncated
/// jar for the loader to find. Inside the instance for the same reason retiring
/// is: a rename is only atomic within one filesystem.
pub const STAGING_DIR: &str = "staging";
/// `.launcher/snapshots/`, containing user-managed local safety copies.
pub const SNAPSHOTS_DIR: &str = "snapshots";
/// `.launcher/put-aside/`, where a pack file the user took out of `mods/` waits
/// to be restored, under its logical path.
///
/// Durable content rather than transaction state: for a file the recipe
/// records no address for, this copy is the only way back.
pub const PUT_ASIDE_DIR: &str = "put-aside";
/// `.launcher/isolation.json`, the active mod isolation session of a host.
///
/// The session names the host it began on and the safety snapshot it will
/// restore, so it is meaningful to that host alone.
pub const ISOLATION_FILE: &str = "isolation.json";
/// `.launcher/isolation-leader.json`, present on a server that an isolation
/// session on its linked client keeps in step.
///
/// It names the client and the server's own safety snapshot. It holds only
/// while that client's session lists the server, so a leftover file never
/// locks a server.
pub const ISOLATION_LEADER_FILE: &str = "isolation-leader.json";
/// The Minecraft game directory inside a client instance root.
///
/// Kept separate from `.launcher/` so that pack extraction, which writes into
/// the game directory, can never reach the launcher's own metadata
pub const CLIENT_GAME_DIR: &str = "minecraft";
/// The server directory inside a server root.
pub const SERVER_GAME_DIR: &str = "server";

/// Failures reading or writing instance metadata.
#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    /// The metadata file could not be written.
    #[error("could not write instance metadata to {path}")]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The metadata file could not be read.
    #[error("could not read instance metadata from {path}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file exists but is not Ephervex metadata.
    #[error("{path} is not valid instance metadata")]
    Invalid {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// The file was written by a newer build.
    #[error(
        "instance metadata at {path} has version {found}, newer than the supported {supported}"
    )]
    UnsupportedVersion {
        path: PathBuf,
        found: u32,
        supported: u32,
    },
    /// A directory could not be created.
    #[error(transparent)]
    Path(#[from] PathError),
}

impl MetadataError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::Unwritable { .. } => "error-registry-metadata-unwritable",
            Self::Unreadable { .. } => "error-registry-metadata-unreadable",
            Self::Invalid { .. } => "error-registry-metadata-invalid",
            Self::UnsupportedVersion { .. } => "error-registry-metadata-unsupported-version",
            Self::Path(error) => error.message_id(),
        }
    }
}

/// The on-disk envelope around a record.
///
/// The version sits beside the payload rather than inside it so a future format
/// change can be detected without first parsing a shape that may have moved.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MetadataFile<T> {
    metadata_version: u32,
    /// Written for humans reading the file in a bug report. Never parsed.
    #[serde(default)]
    written_by: String,
    #[serde(flatten)]
    record: T,
}

impl<T> MetadataFile<T> {
    fn new(record: T) -> Self {
        Self {
            metadata_version: METADATA_VERSION,
            written_by: format!(
                "{} {}",
                ephervex_model::APP_NAME,
                ephervex_model::APP_VERSION
            ),
            record,
        }
    }
}

/// Whether `relative`, a path inside a host root, is launcher-owned state of
/// that host's operations in flight or history: snapshots, retired and staged
/// files, restore journals, or the active isolation session.
///
/// Only entries directly inside `.launcher/` qualify. A game directory may hold
/// folders of the same names, and those are ordinary game content.
pub(crate) fn is_transaction_state(relative: &Path) -> bool {
    let mut components = relative.components();
    if components.next() != Some(Component::Normal(LAUNCHER_METADATA_DIR.as_ref())) {
        return false;
    }
    let Some(Component::Normal(name)) = components.next() else {
        return false;
    };
    name.to_str().is_some_and(|name| {
        matches!(
            name,
            SNAPSHOTS_DIR | RETIRED_DIR | STAGING_DIR | ISOLATION_FILE | ISOLATION_LEADER_FILE
        ) || name.starts_with(crate::restore::RESTORE_PREFIX)
    })
}

/// `<root>/.launcher/`.
#[must_use]
pub fn metadata_dir(root: &Path) -> PathBuf {
    root.join(LAUNCHER_METADATA_DIR)
}

/// `<root>/.launcher/source/`.
#[must_use]
pub fn source_dir(root: &Path) -> PathBuf {
    metadata_dir(root).join(SOURCE_DIR)
}

/// `<root>/.launcher/snapshots/`.
#[must_use]
pub fn snapshots_dir(root: &Path) -> PathBuf {
    metadata_dir(root).join(SNAPSHOTS_DIR)
}

/// `<root>/.launcher/retired/`.
#[must_use]
pub fn retired_dir(root: &Path) -> PathBuf {
    metadata_dir(root).join(RETIRED_DIR)
}

/// `<root>/.launcher/staging/`.
#[must_use]
pub fn staging_dir(root: &Path) -> PathBuf {
    metadata_dir(root).join(STAGING_DIR)
}

/// `<root>/.launcher/put-aside/`.
#[must_use]
pub fn put_aside_dir(root: &Path) -> PathBuf {
    metadata_dir(root).join(PUT_ASIDE_DIR)
}

/// `<root>/.launcher/instance.json`.
#[must_use]
pub fn client_metadata_path(root: &Path) -> PathBuf {
    metadata_dir(root).join(CLIENT_METADATA_FILE)
}

/// `<root>/.launcher/server.json`.
#[must_use]
pub fn server_metadata_path(root: &Path) -> PathBuf {
    metadata_dir(root).join(SERVER_METADATA_FILE)
}

/// `<root>/.launcher/warnings.json`.
#[must_use]
pub fn warnings_path(root: &Path) -> PathBuf {
    metadata_dir(root).join(WARNINGS_FILE)
}

/// `<root>/minecraft/` - where the game actually runs.
#[must_use]
pub fn client_game_dir(root: &Path) -> PathBuf {
    root.join(CLIENT_GAME_DIR)
}

/// `<root>/server/` - where the server actually runs.
#[must_use]
pub fn server_game_dir(root: &Path) -> PathBuf {
    root.join(SERVER_GAME_DIR)
}

/// Whether this directory looks like an Ephervex client instance.
///
/// Used by reconciliation to decide what is worth re-indexing, so it must be
/// cheap and must not care about the contents being current.
#[must_use]
pub fn is_client_instance_root(root: &Path) -> bool {
    client_metadata_path(root).is_file()
}

/// Whether this directory looks like an Ephervex server.
#[must_use]
pub fn is_server_root(root: &Path) -> bool {
    server_metadata_path(root).is_file()
}

/// `<root>/.launcher/incomplete`.
#[must_use]
pub fn incomplete_marker_path(root: &Path) -> PathBuf {
    metadata_dir(root).join(INCOMPLETE_MARKER_FILE)
}

/// Whether this instance root is still being built.
///
/// A read error counts as incomplete. Nothing is gained by treating a root the
/// launcher cannot inspect as finished, and adopting one would be exactly the
/// mistake this marker exists to prevent.
#[must_use]
pub fn is_incomplete(root: &Path) -> bool {
    match std::fs::symlink_metadata(incomplete_marker_path(root)) {
        Ok(_) => true,
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Flags an instance root as still being built.
///
/// Written durably, because the whole point is to survive the loss of power
/// that interrupted the materialization.
pub fn mark_incomplete(root: &Path) -> Result<(), MetadataError> {
    prepare_metadata_dir(root)?;
    let path = incomplete_marker_path(root);
    let file = std::fs::File::create(&path).map_err(|source| MetadataError::Unwritable {
        path: path.clone(),
        source,
    })?;
    file.sync_all()
        .map_err(|source| MetadataError::Unwritable {
            path: path.clone(),
            source,
        })?;
    sync_parent(&path)
}

/// Clears the flag, making the instance publishable.
///
/// A marker that is already gone is success: republishing a complete instance
/// must not fail because there was nothing left to clear.
pub fn clear_incomplete(root: &Path) -> Result<(), MetadataError> {
    let path = incomplete_marker_path(root);
    match std::fs::remove_file(&path) {
        Ok(()) => sync_parent(&path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(MetadataError::Unwritable { path, source }),
    }
}

fn sync_parent(path: &Path) -> Result<(), MetadataError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    // The rename or unlink itself is what has to reach the disk; failing to
    // open the directory is not a reason to fail the operation.
    if let Ok(directory) = std::fs::File::open(parent) {
        directory
            .sync_all()
            .map_err(|source| MetadataError::Unwritable {
                path: parent.to_path_buf(),
                source,
            })?;
    }
    Ok(())
}

/// Writes `.launcher/instance.json` and `.launcher/warnings.json`.
///
/// Call this *before* touching the index (see the module docs). A directory
/// sync failure after a file was published is an error here;
/// [`publish_client_metadata`] reports it separately.
pub fn write_client_metadata(root: &Path, record: &ClientRecord) -> Result<(), MetadataError> {
    let path = client_metadata_path(root);
    publish_client_metadata(root, record)?
        .require_durable()
        .map_err(|source| MetadataError::Unwritable { path, source })
}

/// Writes `.launcher/warnings.json` and `.launcher/instance.json`, reporting a
/// directory sync failure after `instance.json` was published as
/// [`AtomicPublication::Unsynced`].
///
/// `Err` means the previous `instance.json` is still in place.
pub(crate) fn publish_client_metadata(
    root: &Path,
    record: &ClientRecord,
) -> Result<AtomicPublication, MetadataError> {
    prepare_metadata_dir(root)?;
    // `instance.json` is the entity commit point, so it goes last. The
    // warnings file is a derived copy of the record's attention, and a failure
    // writing it after the commit must not make a committed mutation look
    // like a failed one.
    let warnings = publish_json(&warnings_path(root), &record.attention)?;
    let instance = publish_json(&client_metadata_path(root), &MetadataFile::new(record))?;
    Ok(match (instance, warnings) {
        (AtomicPublication::Durable, other) => other,
        (unsynced, _) => unsynced,
    })
}

/// Writes `.launcher/server.json`.
///
/// A directory sync failure after the file was published is an error here;
/// [`publish_server_metadata`] reports it separately.
pub fn write_server_metadata(root: &Path, record: &ServerRecord) -> Result<(), MetadataError> {
    let path = server_metadata_path(root);
    publish_server_metadata(root, record)?
        .require_durable()
        .map_err(|source| MetadataError::Unwritable { path, source })
}

/// Writes `.launcher/server.json`, reporting a directory sync failure after
/// the file was published as [`AtomicPublication::Unsynced`].
///
/// `Err` means the previous `server.json` is still in place.
pub(crate) fn publish_server_metadata(
    root: &Path,
    record: &ServerRecord,
) -> Result<AtomicPublication, MetadataError> {
    prepare_metadata_dir(root)?;
    // `server.json` is the entity commit point. Attention is already part of
    // the record, so a redundant warnings file must not turn a committed
    // mutation into an apparent failure.
    publish_json(&server_metadata_path(root), &MetadataFile::new(record))
}

/// Reads `.launcher/instance.json`.
pub fn read_client_metadata(root: &Path) -> Result<ClientRecord, MetadataError> {
    read_metadata(&client_metadata_path(root))
}

/// Reads `.launcher/server.json`.
pub fn read_server_metadata(root: &Path) -> Result<ServerRecord, MetadataError> {
    read_metadata(&server_metadata_path(root))
}

fn read_metadata<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, MetadataError> {
    let text = std::fs::read_to_string(path).map_err(|source| MetadataError::Unreadable {
        path: path.to_path_buf(),
        source,
    })?;

    // Version first. A newer file may have moved fields this build would
    // otherwise misread as absent.
    let envelope: serde_json::Value =
        serde_json::from_str(&text).map_err(|source| MetadataError::Invalid {
            path: path.to_path_buf(),
            source,
        })?;
    let found = envelope
        .get("metadata_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(METADATA_VERSION);
    if found > METADATA_VERSION {
        return Err(MetadataError::UnsupportedVersion {
            path: path.to_path_buf(),
            found,
            supported: METADATA_VERSION,
        });
    }

    let file: MetadataFile<T> =
        serde_json::from_value(envelope).map_err(|source| MetadataError::Invalid {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(file.record)
}

/// Creates `.launcher/` and `.launcher/source/`.
///
/// `source/` is created empty so importers have somewhere
/// well-known to drop the pack recipe, and so a user browsing the directory can
/// see the shape the launcher expects.
fn prepare_metadata_dir(root: &Path) -> Result<(), MetadataError> {
    create_dir_all(&source_dir(root))?;
    Ok(())
}

fn publish_json<T: Serialize>(path: &Path, value: &T) -> Result<AtomicPublication, MetadataError> {
    // Compact: `.launcher` metadata is read by the launcher, not by a person,
    // and indentation only costs storage and I/O.
    let body = serde_json::to_vec(value).map_err(|error| MetadataError::Unwritable {
        path: path.to_path_buf(),
        source: std::io::Error::other(error),
    })?;

    crate::atomic::publish_bytes_atomic(path, &body, crate::atomic::AtomicWriteOptions::default())
        .map_err(|source| MetadataError::Unwritable {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use chrono::Utc;
    use ephervex_model::{AttentionReason, InstanceId, LoaderKind, LoaderSelection, StorageClass};

    use crate::records::ImportSource;

    fn sample(root: &Path) -> ClientRecord {
        let now = Utc::now();
        ClientRecord {
            id: InstanceId::new(),
            name: "Crash repro".to_owned(),
            root: root.to_path_buf(),
            storage: StorageClass::Persistent,
            minecraft_version: "1.12.2".to_owned(),
            loader: LoaderSelection::exact(LoaderKind::Forge, "14.23.5.2859"),
            source: ImportSource::manual(),
            java_override: None,
            jvm_override: None,
            allow_pack_mod_version_changes: false,
            attention: vec![AttentionReason::IgnoredUnsafeCustomization(
                "pre-launch command".to_owned(),
            )],
            created_at: now,
            updated_at: now,
            last_launched_at: None,
        }
    }

    #[test]
    fn metadata_round_trips_so_a_lost_database_is_recoverable() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path();
        let record = sample(root);

        write_client_metadata(root, &record).expect("write");
        assert_eq!(read_client_metadata(root).expect("read"), record);
    }

    #[test]
    fn metadata_is_stored_compactly() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path();
        write_client_metadata(root, &sample(root)).expect("write");

        for path in [client_metadata_path(root), warnings_path(root)] {
            let text = std::fs::read_to_string(&path).expect("read");
            assert!(
                !text.contains('\n'),
                "{} carries pretty-printing whitespace: {text}",
                path.display()
            );
            serde_json::from_str::<serde_json::Value>(&text).expect("valid JSON");
        }
    }

    #[test]
    fn pretty_printed_metadata_from_an_older_build_still_loads() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path();
        let record = sample(root);
        write_client_metadata(root, &record).expect("write");

        let path = client_metadata_path(root);
        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("parse");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&stored).expect("indent"),
        )
        .expect("rewrite as an older build would have");

        assert_eq!(read_client_metadata(root).expect("read"), record);
    }

    #[test]
    fn the_documented_launcher_directory_layout_is_created() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path();
        write_client_metadata(root, &sample(root)).expect("write");

        assert!(metadata_dir(root).is_dir());
        assert!(
            source_dir(root).is_dir(),
            "importers need a well-known place"
        );
        assert!(client_metadata_path(root).is_file());
        assert!(warnings_path(root).is_file());
        assert!(is_client_instance_root(root));
        assert!(!is_server_root(root));

        // No half-written temporary files left behind.
        assert!(!metadata_dir(root).join("instance.json.tmp").exists());
    }

    #[test]
    fn warnings_are_written_alongside_the_record_rather_than_only_into_the_database() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path();
        let record = sample(root);
        write_client_metadata(root, &record).expect("write");

        let warnings: Vec<AttentionReason> =
            serde_json::from_str(&std::fs::read_to_string(warnings_path(root)).expect("read"))
                .expect("parse");
        assert_eq!(warnings, record.attention);
    }

    #[test]
    fn metadata_from_a_newer_build_is_refused_rather_than_misread() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path();
        write_client_metadata(root, &sample(root)).expect("write");

        let path = client_metadata_path(root);
        let mut stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("parse");
        stored["metadata_version"] = serde_json::json!(99);
        std::fs::write(&path, serde_json::to_vec(&stored).expect("serialize")).expect("write");

        let error = read_client_metadata(root).expect_err("must refuse");
        assert!(matches!(
            error,
            MetadataError::UnsupportedVersion {
                found: 99,
                supported: 1,
                ..
            }
        ));
        assert_eq!(
            error.message_id(),
            "error-registry-metadata-unsupported-version"
        );
    }

    #[test]
    fn a_directory_without_launcher_metadata_is_not_mistaken_for_an_instance() {
        let directory = tempfile::tempdir().expect("temp dir");
        assert!(!is_client_instance_root(directory.path()));
        assert!(!is_server_root(directory.path()));

        let error = read_client_metadata(directory.path()).expect_err("nothing to read");
        assert!(matches!(error, MetadataError::Unreadable { .. }));
    }

    #[test]
    fn corrupt_metadata_reports_the_file_rather_than_panicking() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path();
        create_dir_all(&metadata_dir(root)).expect("mkdir");
        std::fs::write(client_metadata_path(root), "{not json").expect("write");

        let error = read_client_metadata(root).expect_err("must fail");
        assert!(matches!(error, MetadataError::Invalid { .. }));
        assert_eq!(error.message_id(), "error-registry-metadata-invalid");
    }
}
