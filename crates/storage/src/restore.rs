//! Durable replacement transactions used by local snapshot restoration.

use std::path::{Path, PathBuf};

use ephervex_model::SnapshotId;
use serde::{Deserialize, Serialize};

use crate::atomic::{AtomicWriteOptions, sync_parent, write_atomic};
use crate::metadata;

const RESTORE_VERSION: u32 = 1;
const JOURNAL_FILE: &str = "restore.json";
pub(crate) const RESTORE_PREFIX: &str = "restore-";

/// A fixed host part that restoration may replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestorePart {
    ClientGame,
    ServerGame,
    Source,
    ClientMetadata,
    ServerMetadata,
    /// The pack files the user put aside, which are what makes their records
    /// restorable.
    PutAside,
}

impl RestorePart {
    fn relative_path(self) -> &'static Path {
        match self {
            Self::ClientGame => Path::new(metadata::CLIENT_GAME_DIR),
            Self::ServerGame => Path::new(metadata::SERVER_GAME_DIR),
            Self::Source => Path::new(".launcher/source"),
            Self::ClientMetadata => Path::new(".launcher/instance.json"),
            Self::ServerMetadata => Path::new(".launcher/server.json"),
            Self::PutAside => Path::new(".launcher/put-aside"),
        }
    }
}

/// One replacement and whether either side existed before publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestorePartState {
    pub part: RestorePart,
    pub original_present: bool,
    pub replacement_present: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct RestoreJournal {
    restore_version: u32,
    parts: Vec<RestorePartState>,
    #[serde(default)]
    committed: bool,
}

/// A prepared restore transaction inside one host's staging directory.
#[derive(Debug, Clone)]
pub struct RestoreTransaction {
    root: PathBuf,
    directory: PathBuf,
    parts: Vec<RestorePartState>,
}

impl RestoreTransaction {
    #[must_use]
    pub fn incoming_path(&self, part: RestorePart) -> PathBuf {
        self.directory.join("incoming").join(part.relative_path())
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Replaces every captured part. Failure attempts a complete rollback.
    pub fn publish(&self) -> std::io::Result<()> {
        metadata::mark_incomplete(&self.root).map_err(std::io::Error::other)?;
        for state in &self.parts {
            if let Err(error) = publish_part(&self.root, &self.directory, state) {
                let rollback = rollback_transaction(&self.root, &self.directory, &self.parts);
                return match rollback {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(std::io::Error::other(format!(
                        "restore failed: {error}; rollback failed: {rollback}"
                    ))),
                };
            }
        }
        Ok(())
    }

    /// Makes a successfully published restore authoritative and removes the
    /// pre-restore trees.
    pub fn finish(&self) -> std::io::Result<()> {
        // The decision must survive a crash before the marker or old trees
        // disappear. Recovery must never roll a committed replacement back.
        self.record_commit()?;
        metadata::clear_incomplete(&self.root).map_err(std::io::Error::other)?;
        remove_transaction(&self.directory)
    }

    fn record_commit(&self) -> std::io::Result<()> {
        let journal = RestoreJournal {
            restore_version: RESTORE_VERSION,
            parts: self.parts.clone(),
            committed: true,
        };
        write_atomic(
            &self.directory.join(JOURNAL_FILE),
            AtomicWriteOptions::private(),
            |file| serde_json::to_writer(file, &journal).map_err(std::io::Error::other),
        )
    }

    /// Restores every pre-transaction part and removes the journal.
    pub fn rollback(&self) -> std::io::Result<()> {
        rollback_transaction(&self.root, &self.directory, &self.parts)
    }

    /// Removes a prepared transaction before publication has begun.
    pub fn abort(&self) -> std::io::Result<()> {
        remove_transaction(&self.directory)
    }
}

/// Creates a durable journal before any live path is moved.
///
/// The caller holds the host's claim until the transaction ends.
pub fn prepare_restore(
    root: &Path,
    id: SnapshotId,
    parts: Vec<RestorePartState>,
) -> std::io::Result<RestoreTransaction> {
    let staging = metadata::staging_dir(root);
    match std::fs::read_dir(&staging) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "another interrupted host mutation requires attention",
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let directory = staging.join(format!("{RESTORE_PREFIX}{id}"));
    std::fs::create_dir_all(&directory)?;
    let journal = RestoreJournal {
        restore_version: RESTORE_VERSION,
        parts: parts.clone(),
        committed: false,
    };
    let path = directory.join(JOURNAL_FILE);
    write_atomic(&path, AtomicWriteOptions::private(), |file| {
        serde_json::to_writer(file, &journal).map_err(std::io::Error::other)
    })?;
    Ok(RestoreTransaction {
        root: root.to_path_buf(),
        directory,
        parts,
    })
}

/// Rolls back every recognized interrupted restore under a host root.
///
/// Every transaction found is treated as abandoned, so the caller must hold the
/// host's claim: a restore still running holds it until it ends.
///
/// An invalid or unfamiliar staging directory is left untouched and returns
/// an error, allowing reconciliation to surface it as suspicious state.
pub fn recover_interrupted_restores(root: &Path) -> std::io::Result<usize> {
    let staging = metadata::staging_dir(root);
    let entries = match std::fs::read_dir(&staging) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut recovered = 0usize;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir()
            || !entry
                .file_name()
                .to_string_lossy()
                .starts_with(RESTORE_PREFIX)
        {
            continue;
        }
        let directory = entry.path();
        // Cleanup removes the journal last. An empty directory with no live
        // transaction marker can only need directory cleanup.
        if !metadata::is_incomplete(root) && std::fs::read_dir(&directory)?.next().is_none() {
            std::fs::remove_dir(&directory)?;
            sync_parent(&directory)?;
            continue;
        }
        let journal = read_journal(&directory)?;
        if journal.committed {
            metadata::clear_incomplete(root).map_err(std::io::Error::other)?;
            remove_transaction(&directory)?;
        } else if metadata::is_incomplete(root) {
            rollback_transaction(root, &directory, &journal.parts)?;
            recovered += 1;
        } else {
            // Publication always marks the host incomplete before its first
            // rename. A prepared transaction is safe to discard.
            remove_transaction(&directory)?;
        }
    }
    Ok(recovered)
}

fn read_journal(directory: &Path) -> std::io::Result<RestoreJournal> {
    let bytes = std::fs::read(directory.join(JOURNAL_FILE))?;
    let journal: RestoreJournal = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
    if journal.restore_version != RESTORE_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unsupported restore journal version",
        ));
    }
    Ok(journal)
}

fn publish_part(root: &Path, directory: &Path, state: &RestorePartState) -> std::io::Result<()> {
    let relative = state.part.relative_path();
    let live = root.join(relative);
    let incoming = directory.join("incoming").join(relative);
    let replaced = directory.join("replaced").join(relative);
    if state.original_present {
        create_parent(&replaced)?;
        std::fs::rename(&live, &replaced)?;
        sync_parent(&live)?;
        sync_parent(&replaced)?;
    }
    if state.replacement_present {
        create_parent(&live)?;
        std::fs::rename(&incoming, &live)?;
        sync_parent(&live)?;
        sync_parent(&incoming)?;
    }
    Ok(())
}

fn rollback_transaction(
    root: &Path,
    directory: &Path,
    parts: &[RestorePartState],
) -> std::io::Result<()> {
    for state in parts.iter().rev() {
        let relative = state.part.relative_path();
        let live = root.join(relative);
        let incoming = directory.join("incoming").join(relative);
        let replaced = directory.join("replaced").join(relative);
        if state.original_present {
            if replaced.exists() || std::fs::symlink_metadata(&replaced).is_ok() {
                remove_path_if_present(&live)?;
                create_parent(&live)?;
                std::fs::rename(&replaced, &live)?;
                sync_parent(&live)?;
            }
        } else if state.replacement_present && !incoming.exists() {
            remove_path_if_present(&live)?;
        }
    }
    metadata::clear_incomplete(root).map_err(std::io::Error::other)?;
    remove_transaction(directory)
}

fn remove_transaction(directory: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_name() != JOURNAL_FILE {
            remove_path_if_present(&entry.path())?;
        }
    }
    std::fs::File::open(directory)?.sync_all()?;
    std::fs::remove_file(directory.join(JOURNAL_FILE))?;
    std::fs::remove_dir(directory)?;
    sync_parent(directory)
}

fn create_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn remove_path_if_present(path: &Path) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        crate::removal::remove_tree_if_present(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn a_prepared_restore_is_discarded_without_changing_live_files() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path();
        std::fs::create_dir(root.join("minecraft")).expect("game");
        std::fs::write(root.join("minecraft/state"), b"live").expect("state");
        let transaction = prepare_restore(root, SnapshotId::new(), vec![]).expect("prepare");
        assert_eq!(recover_interrupted_restores(root).expect("recover"), 0);
        assert!(!transaction.directory().exists());
        assert_eq!(
            std::fs::read(root.join("minecraft/state")).expect("state"),
            b"live"
        );
    }

    #[test]
    fn committed_restore_cleanup_keeps_the_replacement_after_restart() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path();
        let live = root.join("minecraft");
        std::fs::create_dir(&live).expect("game");
        std::fs::write(live.join("state"), b"old").expect("state");
        let transaction = prepare_restore(
            root,
            SnapshotId::new(),
            vec![RestorePartState {
                part: RestorePart::ClientGame,
                original_present: true,
                replacement_present: true,
            }],
        )
        .expect("prepare");
        let incoming = transaction.incoming_path(RestorePart::ClientGame);
        std::fs::create_dir_all(&incoming).expect("incoming");
        std::fs::write(incoming.join("state"), b"restored").expect("state");
        transaction.publish().expect("publish");
        transaction.record_commit().expect("commit");
        assert_eq!(recover_interrupted_restores(root).expect("recover"), 0);
        assert_eq!(
            std::fs::read(live.join("state")).expect("state"),
            b"restored"
        );
        assert!(!metadata::is_incomplete(root));
        assert!(!transaction.directory().exists());
    }

    #[test]
    fn interrupted_publication_restores_original_parts_on_restart() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("host");
        let live = root.join("minecraft");
        std::fs::create_dir_all(&live).expect("create live");
        std::fs::write(live.join("state.txt"), b"before").expect("write original");
        let transaction = prepare_restore(
            &root,
            SnapshotId::new(),
            vec![RestorePartState {
                part: RestorePart::ClientGame,
                original_present: true,
                replacement_present: true,
            }],
        )
        .expect("prepare restore");
        let incoming = transaction.incoming_path(RestorePart::ClientGame);
        std::fs::create_dir_all(&incoming).expect("create incoming");
        std::fs::write(incoming.join("state.txt"), b"snapshot").expect("write replacement");
        metadata::mark_incomplete(&root).expect("mark incomplete");
        let replaced = transaction.directory().join("replaced").join("minecraft");
        std::fs::create_dir_all(replaced.parent().expect("parent")).expect("create replaced");
        std::fs::rename(&live, &replaced).expect("retire live");
        std::fs::rename(&incoming, &live).expect("publish replacement");

        assert_eq!(recover_interrupted_restores(&root).expect("recover"), 1);
        assert_eq!(
            std::fs::read(live.join("state.txt")).expect("read"),
            b"before"
        );
        assert!(!metadata::is_incomplete(&root));
        assert!(!transaction.directory().exists());
    }

    fn read_only_game(live: &Path) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::create_dir_all(live.join("frozen")).expect("frozen");
        std::fs::write(live.join("frozen/file"), b"old").expect("file");
        std::fs::set_permissions(live.join("frozen"), std::fs::Permissions::from_mode(0o555))
            .expect("chmod");
    }

    fn prepared_client_game_restore(root: &Path) -> RestoreTransaction {
        let transaction = prepare_restore(
            root,
            SnapshotId::new(),
            vec![RestorePartState {
                part: RestorePart::ClientGame,
                original_present: true,
                replacement_present: true,
            }],
        )
        .expect("prepare");
        let incoming = transaction.incoming_path(RestorePart::ClientGame);
        std::fs::create_dir_all(&incoming).expect("incoming");
        std::fs::write(incoming.join("state"), b"restored").expect("state");
        transaction
    }

    #[test]
    fn finishing_a_restore_removes_a_replaced_tree_with_a_read_only_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path();
        let live = root.join("minecraft");
        read_only_game(&live);
        let transaction = prepared_client_game_restore(root);
        transaction.publish().expect("publish");

        let finished = transaction.finish();

        crate::removal::remove_tree_if_present(root).ok();
        finished.expect("the replaced tree is removed despite its mode");
    }

    #[test]
    fn rolling_back_a_restore_removes_a_published_tree_with_a_read_only_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path();
        let live = root.join("minecraft");
        std::fs::create_dir_all(&live).expect("live");
        std::fs::write(live.join("state"), b"old").expect("state");
        let transaction = prepared_client_game_restore(root);
        let incoming = transaction.incoming_path(RestorePart::ClientGame);
        read_only_game(&incoming);
        transaction.publish().expect("publish");

        let rolled_back = transaction.rollback();

        let original = std::fs::read(live.join("state")).ok();
        crate::removal::remove_tree_if_present(root).ok();
        rolled_back.expect("the published tree is removed despite its mode");
        assert_eq!(original.as_deref(), Some(b"old".as_slice()));
    }
}
