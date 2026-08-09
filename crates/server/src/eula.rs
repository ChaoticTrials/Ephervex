//! Explicit global Minecraft EULA consent and per-start file materialization.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::properties::PropertiesDocument;
use crate::{ServerError, ServerResult};

/// The stable reference shown before consent is accepted.
pub const MINECRAFT_EULA_REFERENCE: &str = "https://aka.ms/MinecraftEULA";
/// The phrase the interactive CLI requires.
pub const EULA_CONFIRMATION_PHRASE: &str = "I ACCEPT";

/// The persisted global consent fields, independent of the config file shape.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EulaConsentRecord {
    pub accepted: bool,
    pub accepted_at: String,
    pub reference: String,
}

/// The interpreted global consent state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EulaStatus {
    NotAccepted,
    Accepted {
        accepted_at: DateTime<Utc>,
        reference: String,
    },
    Malformed,
}

impl EulaStatus {
    #[must_use]
    pub fn from_record(record: &EulaConsentRecord) -> Self {
        if !record.accepted {
            return Self::NotAccepted;
        }
        let accepted_at = DateTime::parse_from_rfc3339(&record.accepted_at)
            .map(|timestamp| timestamp.with_timezone(&Utc));
        match accepted_at {
            Ok(accepted_at) if record.reference == MINECRAFT_EULA_REFERENCE => Self::Accepted {
                accepted_at,
                reference: record.reference.clone(),
            },
            _ => Self::Malformed,
        }
    }

    pub fn require_valid(&self) -> ServerResult<()> {
        match self {
            Self::Accepted { .. } => Ok(()),
            Self::NotAccepted => Err(ServerError::EulaNotAccepted),
            Self::Malformed => Err(ServerError::EulaConsentMalformed),
        }
    }
}

/// Creates the exact record written after interactive confirmation.
#[must_use]
pub fn accepted_record(at: DateTime<Utc>) -> EulaConsentRecord {
    EulaConsentRecord {
        accepted: true,
        accepted_at: at.to_rfc3339(),
        reference: MINECRAFT_EULA_REFERENCE.to_owned(),
    }
}

/// A revoked record retains no stale authorization material.
#[must_use]
pub fn revoked_record() -> EulaConsentRecord {
    EulaConsentRecord::default()
}

/// Atomically merges `eula=true` only after valid global consent is supplied.
pub fn materialize_eula_file(path: &Path, status: &EulaStatus) -> ServerResult<()> {
    status.require_valid()?;
    let original = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(ServerError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut document = PropertiesDocument::parse(&original);
    document.merge_eula_accepted();
    atomic_write(path, document.render().as_bytes())
}

fn atomic_write(path: &Path, contents: &[u8]) -> ServerResult<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|source| ServerError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let temporary = sibling_temporary(path);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|source| ServerError::Io {
            path: temporary.clone(),
            source,
        })?;
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|source| ServerError::Io {
            path: temporary.clone(),
            source,
        })?;
    std::fs::rename(&temporary, path).map_err(|source| ServerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ServerError::Io {
            path: parent.to_path_buf(),
            source,
        })
}

fn sibling_temporary(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("eula.txt");
    path.with_file_name(format!(".{name}.tmp"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn only_a_current_complete_record_is_valid_consent() {
        assert_eq!(
            EulaStatus::from_record(&EulaConsentRecord::default()),
            EulaStatus::NotAccepted
        );
        let accepted = accepted_record(Utc::now());
        assert!(matches!(
            EulaStatus::from_record(&accepted),
            EulaStatus::Accepted { .. }
        ));
        let mut malformed = accepted;
        malformed.reference = "https://example.invalid".to_owned();
        assert_eq!(EulaStatus::from_record(&malformed), EulaStatus::Malformed);
    }

    #[test]
    fn no_eula_file_is_written_without_global_consent() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("eula.txt");
        let error = materialize_eula_file(&path, &EulaStatus::NotAccepted)
            .expect_err("consent is required");
        assert!(matches!(error, ServerError::EulaNotAccepted));
        assert!(!path.exists());
    }

    #[test]
    fn accepted_eula_is_merged_without_discarding_existing_content() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("eula.txt");
        std::fs::write(&path, "# generated\neula=false\nunknown=yes\n").expect("seed");
        let status = EulaStatus::from_record(&accepted_record(Utc::now()));
        materialize_eula_file(&path, &status).expect("write");
        assert_eq!(
            std::fs::read_to_string(path).expect("read"),
            "# generated\neula=true\nunknown=yes\n"
        );
    }
}
