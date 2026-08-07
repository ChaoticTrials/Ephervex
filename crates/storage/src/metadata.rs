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
//! reconstruct the record — because it is sitting in the directory they care
//! about.
//!
//! # Write order
//!
//! Metadata is written *before* the index row, always. A crash between the two
//! leaves a directory that startup reconciliation will re-index; the reverse
//! order would leave an index row pointing at nothing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
/// `.launcher/source/`, holding the provider-specific recipe.
pub const SOURCE_DIR: &str = "source";
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

/// `<root>/minecraft/` — where the game actually runs.
#[must_use]
pub fn client_game_dir(root: &Path) -> PathBuf {
    root.join(CLIENT_GAME_DIR)
}

/// `<root>/server/` — where the server actually runs.
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

/// Writes `.launcher/instance.json` and `.launcher/warnings.json`.
///
/// Call this *before* touching the index (see the module docs).
pub fn write_client_metadata(root: &Path, record: &ClientRecord) -> Result<(), MetadataError> {
    prepare_metadata_dir(root)?;
    write_json(&client_metadata_path(root), &MetadataFile::new(record))?;
    write_json(&warnings_path(root), &record.attention)
}

/// Writes `.launcher/server.json` and `.launcher/warnings.json`.
pub fn write_server_metadata(root: &Path, record: &ServerRecord) -> Result<(), MetadataError> {
    prepare_metadata_dir(root)?;
    write_json(&server_metadata_path(root), &MetadataFile::new(record))?;
    write_json(&warnings_path(root), &record.attention)
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

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), MetadataError> {
    // Pretty-printed on purpose: these files end up pasted into bug reports.
    let text = serde_json::to_string_pretty(value).map_err(|error| MetadataError::Unwritable {
        path: path.to_path_buf(),
        source: std::io::Error::other(error),
    })?;

    // Same rename dance as the configuration file: an interrupted write must
    // not be able to turn a recoverable instance into an unparseable one.
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text).map_err(|source| MetadataError::Unwritable {
        path: temporary.clone(),
        source,
    })?;
    std::fs::rename(&temporary, path).map_err(|source| MetadataError::Unwritable {
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
        let text = std::fs::read_to_string(&path)
            .expect("read")
            .replace("\"metadata_version\": 1", "\"metadata_version\": 99");
        std::fs::write(&path, text).expect("write");

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
