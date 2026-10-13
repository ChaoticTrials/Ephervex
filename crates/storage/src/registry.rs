//! Create, read, list, update, and delete instances and servers.
//!
//! The registry is the only thing in the workspace that writes to both halves
//! of ADR 0002 - the `.launcher/*.json` metadata and the SQLite index - and it
//! always writes them in that order. A crash between the two leaves a directory
//! that [`crate::reconcile`] will pick up and re-index on the next start; the
//! reverse order would leave an index row pointing at a directory that was
//! never created.
//!
//! # An instance is not complete until it is published
//!
//! Creating a client instance and *finishing* one are separated, because the
//! work in between (laying down a pack, downloading mods, applying
//! preferences) takes as long as it takes and can be interrupted.
//! [`Registry::create_client`] therefore writes no index row at all, and
//! [`Registry::publish_client`] is what adds it.
//!
//! That separation is the enforcement, not a convention callers are asked to
//! respect. Every read, listing, launch, and mutation path in this module
//! resolves through the index, so an unpublished instance is *absent* from all
//! of them; there is no state in which one of them can observe a half-built
//! instance and mistake it for a finished one. Reconciliation is the one thing
//! that reads directories instead of the index, so it gets a second, separate
//! signal: [`metadata::INCOMPLETE_MARKER_FILE`], written at creation and
//! cleared at publication.
//!
//! Both orderings around the marker are deliberate.
//!
//! The marker is written *before* the metadata it guards. `instance.json` is a
//! durable write, so writing it first would leave a window in which a crash
//! produces a directory that parses as an instance and carries no marker,
//! which is precisely the state reconciliation is right to adopt. A crash in
//! the order used here leaves a marked directory with no metadata, which is
//! not an instance root at all and is left alone.
//!
//! The marker is cleared *before* the row is inserted. A crash between those
//! two leaves a markerless directory with no row, which reconciliation adopts,
//! correct because everything was materialized. The reverse order could index
//! a row for a root still flagged incomplete.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ephervex_model::{
    AttentionReason, ImportSourceKind, InstanceId, LoaderKind, LoaderSelection, ServerId,
    ServerInstallation, StorageClass,
};
use rusqlite::{Row, params};

use crate::atomic::AtomicPublication;
use crate::index::{Index, IndexError};
use crate::lock::{FileGuard, GuardError, clear_launch_locks_of_idle_instance, discard_guard};
use crate::metadata::{self, MetadataError};
use crate::paths::{PathError, Paths, create_dir_all};
use crate::reconcile::RecordRef;
use crate::records::{
    ClientCommit, ClientCommitWarning, ClientRecord, DUPLICATE_NAME_SUFFIX, DuplicateClient,
    DuplicatedClient, FileDisposition, ImportSource, JavaOverride, JvmSettings, NewClient,
    NewServer, ServerCommit, ServerCommitWarning, ServerCreationSource, ServerNetworkSettings,
    ServerRecord,
};

const CLIENT_COLUMNS: &str = "id, name, root, storage_class, minecraft_version, loader_kind, \
     loader_version, source_kind, source_reference, java_override, jvm_override, \
     allow_pack_mod_version_changes, attention, created_at, updated_at, last_launched_at";

const SERVER_COLUMNS: &str = "id, name, root, storage_class, linked_client_id, minecraft_version, \
     loader_kind, loader_version, network, eula_accepted, source_kind, source_reference, \
     java_override, jvm_override, attention, created_at, updated_at, last_started_at, installation, \
     operator_setup";

/// Failures from registry operations.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// No instance with that id is indexed.
    #[error("no instance with id {id}")]
    InstanceUnknown { id: InstanceId },
    /// No server with that id is indexed.
    #[error("no server with id {id}")]
    ServerUnknown { id: ServerId },
    /// The metadata at an indexed instance root names a different instance,
    /// as when the directory was copied from another one.
    #[error("the metadata at {path} belongs to a different instance")]
    InstanceMetadataForeign { path: PathBuf },
    /// The same for a server.
    #[error("the metadata at {path} belongs to a different server")]
    ServerMetadataForeign { path: PathBuf },
    /// The directory chosen for a new instance already has contents.
    #[error("the instance directory {path} already contains files")]
    RootOccupied { path: PathBuf },
    /// An interrupted snapshot restore of this instance or server has not been
    /// rolled back yet.
    #[error("{path} has an interrupted snapshot restore that startup has not recovered")]
    RestoreUnfinished { path: PathBuf },
    /// Another change to this instance or server, or its start, is still
    /// running.
    #[error("{path} is being changed or started by another operation")]
    HostBusy { path: PathBuf },
    /// Another launcher process is changing this host's recipe and did not
    /// finish within the time a caller that cannot be canceled waits.
    #[error("{path} has a mod change in progress in another Ephervex process")]
    RecipeBusy { path: PathBuf },
    /// The file that keeps changes to one host apart could not be used.
    #[error("the host guard at {path} could not be used")]
    HostGuardUnusable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The instance is indexed but its files are gone.
    ///
    /// Distinct from [`Self::InstanceUnknown`] on purpose: the record is right
    /// there, and the user needs to hear that the *directory* vanished - the
    /// state reconciliation deliberately preserves rather than deletes
    #[error("the instance directory {path} no longer exists")]
    RootMissing { path: PathBuf },
    /// A filesystem operation failed.
    #[error("filesystem error at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The claim handed to a change names a different host.
    #[error("the claim does not cover {path}")]
    HostNotClaimed { path: PathBuf },
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Metadata(#[from] MetadataError),
    #[error(transparent)]
    Path(#[from] PathError),
    /// A launch holds the instance, so it cannot be moved.
    #[error(transparent)]
    Lock(#[from] crate::lock::LockError),
}

impl RegistryError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::InstanceUnknown { .. } => "error-registry-instance-unknown",
            Self::ServerUnknown { .. } => "error-registry-server-unknown",
            Self::InstanceMetadataForeign { .. } => "error-registry-instance-metadata-foreign",
            Self::ServerMetadataForeign { .. } => "error-registry-server-metadata-foreign",
            Self::RootOccupied { .. } => "error-registry-root-occupied",
            Self::RestoreUnfinished { .. } => "error-registry-restore-unfinished",
            Self::HostBusy { .. } => "error-registry-host-busy",
            Self::RecipeBusy { .. } => "error-registry-recipe-busy",
            Self::HostGuardUnusable { .. } => "error-registry-host-guard-unusable",
            Self::RootMissing { .. } => "error-registry-root-missing",
            Self::HostNotClaimed { .. } => "error-registry-host-not-claimed",
            Self::Io { .. } => "error-paths-directory-creation",
            Self::Index(error) => error.message_id(),
            Self::Metadata(error) => error.message_id(),
            Self::Path(error) => error.message_id(),
            Self::Lock(error) => error.message_id(),
        }
    }
}

impl From<rusqlite::Error> for RegistryError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Index(IndexError::from(error))
    }
}

/// The instance and server registry.
///
/// Owns the resolved [`Paths`] rather than re-deriving them, so a process
/// cannot end up creating instances in one place and looking for them in
/// another.
#[derive(Debug)]
pub struct Registry {
    paths: Paths,
    index: Index,
}

impl Registry {
    /// Opens the registry against the database named by `paths`.
    ///
    /// Migrations run here. Startup reconciliation does not - that is a
    /// separate, explicitly invoked step so that a caller which only wants to
    /// read cannot accidentally mutate the index (see [`crate::reconcile`]).
    pub fn open(paths: Paths) -> Result<Self, RegistryError> {
        let index = Index::open(&paths.database_file())?;
        Ok(Self { paths, index })
    }

    /// Opens a registry backed by an in-memory index.
    ///
    /// Instances still land on disk under `paths`; only the index is
    /// throwaway. Used by tests and by "inspect this directory without
    /// touching my real index" tooling.
    pub fn open_in_memory(paths: Paths) -> Result<Self, RegistryError> {
        Ok(Self {
            paths,
            index: Index::open_in_memory()?,
        })
    }

    /// The resolved application paths this registry writes through.
    #[must_use]
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// The underlying index, for schema inspection.
    #[must_use]
    pub fn index(&self) -> &Index {
        &self.index
    }

    // ---------------------------------------------------------------- clients

    /// Creates a client instance: directory, metadata, then index row.
    pub fn create_client(&self, request: NewClient) -> Result<ClientRecord, RegistryError> {
        let id = InstanceId::new();
        let root = self.paths.instance_root(request.storage, id);
        ensure_free_root(&root)?;

        let now = Utc::now();
        let record = ClientRecord {
            id,
            name: request.name,
            root: root.clone(),
            storage: request.storage,
            minecraft_version: request.minecraft_version,
            loader: request.loader,
            // An instance created here has no pack declaration to preserve,
            // so its mods are the user's from the start. An imported one
            // starts off: see the field's own documentation.
            allow_pack_mod_version_changes: request.source.is_manual(),
            source: request.source,
            java_override: None,
            jvm_override: None,
            attention: request.attention,
            created_at: now,
            updated_at: now,
            last_launched_at: None,
        };

        create_dir_all(&metadata::client_game_dir(&root))?;
        // Marker before metadata. See the module documentation: the reverse
        // order leaves a window in which a crash produces a directory that
        // parses as a finished instance and carries nothing to say it is not.
        metadata::mark_incomplete(&root)?;
        metadata::write_client_metadata(&root, &record)?;
        // No index row yet: the caller still has to materialize this instance,
        // and until it does the instance must not exist as far as every other
        // entry point is concerned.

        tracing::info!(
            instance_id = %record.id,
            storage = %record.storage,
            root = %record.root.display(),
            "created client instance"
        );
        Ok(record)
    }

    /// Finishes a created instance, making it visible to every other operation.
    ///
    /// The last step of creating, importing, or duplicating a client. Until it
    /// succeeds the instance has no index row, so nothing can list it, read it,
    /// launch it, or mutate it.
    ///
    /// Idempotent for the marker's sake, since publishing an instance whose
    /// marker is already gone is not an error, but not for the row insert: two
    /// publications of one id is a caller bug rather than a recoverable state.
    pub fn publish_client(&self, record: &ClientRecord) -> Result<(), RegistryError> {
        // Marker first. See the module documentation.
        metadata::clear_incomplete(&record.root)?;
        self.insert_client_row(record)?;
        tracing::info!(
            instance_id = %record.id,
            root = %record.root.display(),
            "published client instance"
        );
        Ok(())
    }

    /// Creates and publishes in one step.
    ///
    /// For tests that need an ordinary finished instance as a fixture. Tests
    /// that are about the create/publish boundary itself call the two halves
    /// separately, which is the only way to observe an unpublished instance.
    #[cfg(test)]
    pub(crate) fn create_published_client(
        &self,
        request: NewClient,
    ) -> Result<ClientRecord, RegistryError> {
        let record = self.create_client(request)?;
        self.publish_client(&record)?;
        Ok(record)
    }

    /// Removes an instance that was created but never published.
    ///
    /// The compensating action for a failed materialization. It takes the
    /// record rather than an id precisely because there is no row to look one
    /// up by, and it removes the directory the caller was handed and nothing
    /// else.
    pub fn discard_unpublished_client(&self, record: &ClientRecord) -> Result<(), RegistryError> {
        remove_tree(&record.root)?;
        tracing::info!(
            instance_id = %record.id,
            root = %record.root.display(),
            "discarded an unpublished client instance"
        );
        Ok(())
    }

    /// Adds an already-existing instance directory to the index.
    ///
    /// Used by reconciliation when a `.launcher/instance.json` is found that
    /// the database does not know about - the recovery path ADR 0002 exists
    /// for. The metadata file is left exactly as found.
    pub fn index_existing_client(&self, record: &ClientRecord) -> Result<(), RegistryError> {
        self.insert_client_row(record)
    }

    /// Moves a client instance into a different storage root, unchanged.
    ///
    /// The opposite of [`Self::duplicate_client`] in the thing that matters:
    /// this is the same instance at a new address. Its id, name, retained
    /// recipe, launch history and linked servers are all untouched, and only
    /// `root` and `storage` differ afterwards (ADR 0025).
    ///
    /// The move is a rename where a rename works and a symlink-preserving copy
    /// where the roots are on different filesystems, which is the ordinary case
    /// between a tmpfs temporary root and a data root on disk.
    ///
    /// The caller holds the instance's [`HostClaim`] and has checked, under it,
    /// that the game is stopped: a launch cannot begin while the claim is held.
    /// A launch that holds the instance anyway refuses the move, and a lock left
    /// by a launch that is over is dropped rather than carried along.
    pub fn relocate_client(
        &self,
        id: InstanceId,
        claim: &HostClaim,
        storage: StorageClass,
        progress: &mut dyn FnMut(usize),
    ) -> Result<ClientCommit<ClientRecord>, RegistryError> {
        // The row as indexed, so a retry after a failed index write still
        // finds the instance in its old storage class and repairs the row.
        let record = self.indexed_client(id)?;
        claim.require(RecordRef::Client(id), &record.root)?;
        if record.storage == storage {
            return Ok(ClientCommit::clean(record));
        }

        let destination = self.paths.instance_root(storage, id);
        let names_this_instance =
            |root: &Path| metadata::read_client_metadata(root).is_ok_and(|found| found.id == id);
        let source = record.root.clone();
        refuse_unhealthy_source(&source, &destination, |root| {
            canonical_client_at(root, id).map(drop)
        })?;
        let (committed, report) = move_host(
            &source,
            &destination,
            names_this_instance,
            clear_launch_locks_of_idle_instance,
            progress,
            || {
                // Edits stored while the files moved are in the metadata, not
                // in the record read before the move.
                let mut current = canonical_client_at(host_files_root(&source, &destination), id)?;
                current.root.clone_from(&destination);
                current.storage = storage;
                let warnings = self.update_client_committed(&mut current)?;
                Ok(ClientCommit {
                    record: current,
                    warnings,
                })
            },
        )?;
        let record = &committed.record;

        tracing::info!(
            instance_id = %id,
            storage = %record.storage,
            root = %record.root.display(),
            renamed = report.renamed,
            files = report.files,
            links = report.links,
            "moved a client instance to a different storage root"
        );
        Ok(committed)
    }

    /// The same move for a dedicated server.
    ///
    /// A server's `libraries` is a link into the shared library store, which is
    /// why the move recreates links rather than copying what they point at.
    ///
    /// The caller holds the server's [`HostClaim`] and has checked, under it,
    /// that the server is stopped.
    pub fn relocate_server(
        &self,
        id: ServerId,
        claim: &HostClaim,
        storage: StorageClass,
        progress: &mut dyn FnMut(usize),
    ) -> Result<ServerCommit<ServerRecord>, RegistryError> {
        let record = self.server(id)?;
        claim.require(RecordRef::Server(id), &record.root)?;
        if record.storage == storage {
            return Ok(ServerCommit {
                record,
                warnings: Vec::new(),
            });
        }

        let destination = self.paths.server_root(storage, id);
        let names_this_server =
            |root: &Path| metadata::read_server_metadata(root).is_ok_and(|found| found.id == id);

        let source = record.root.clone();
        refuse_unhealthy_source(&source, &destination, |root| {
            canonical_server_at(root, id).map(drop)
        })?;
        // `server.json` is the commit point: an index failure after it is a
        // warning on a recorded move, not a failed one.
        let (commit, report) = move_host(
            &source,
            &destination,
            names_this_server,
            |_| Ok(()),
            progress,
            || {
                // The canonical file wins over the index row, which can lag it
                // (ADR 0002).
                let mut current = canonical_server_at(host_files_root(&source, &destination), id)?;
                current.root.clone_from(&destination);
                current.storage = storage;
                self.update_server_committed(&mut current)
            },
        )?;

        tracing::info!(
            server_id = %id,
            storage = %commit.record.storage,
            root = %commit.record.root.display(),
            renamed = report.renamed,
            "moved a server to a different storage root"
        );
        Ok(commit)
    }

    /// Duplicates a client instance, files and all.
    ///
    /// The copy is a *new instance that happens to contain the same files*: it
    /// gets its own id, its own root, its own creation time, and no launch
    /// history. Everything that describes what the instance **is** - Minecraft
    /// version, loader, import source, Java and JVM overrides, warnings - is
    /// preserved, because a duplicate that reproduces something different from
    /// its original is not a duplicate.
    ///
    /// What is deliberately not carried over:
    ///
    /// * **launch locks**, which describe running processes;
    /// * **`last_launched_at`**, for the same reason;
    /// * **launcher transaction state** directly inside `.launcher/`:
    ///   snapshots, retired and staged files, restore journals, and an active
    ///   isolation session, which names the original host and a snapshot the
    ///   copy does not have. Game folders with those names are copied;
    /// * **linked servers** - a server is its own entity with its own world
    ///   and its own process, and silently forking one because a client
    ///   was copied would be a surprising amount of disk to spend;
    /// * **global preferences**, which are *not* re-applied. Ephervex is explicit:
    ///   the copy keeps the options file the original had, so duplicating an
    ///   instance whose settings were tuned by hand does not quietly reset
    ///   them.
    pub fn duplicate_client(
        &self,
        source: InstanceId,
        request: &DuplicateClient,
    ) -> Result<DuplicatedClient, RegistryError> {
        let original = self.current_client(source)?;
        if !original.root.is_dir() {
            return Err(RegistryError::RootMissing {
                path: original.root.clone(),
            });
        }

        let storage = request.storage.unwrap_or(original.storage);
        let id = InstanceId::new();
        let root = self.paths.instance_root(storage, id);
        ensure_free_root(&root)?;

        // Before the copy, not after it: the copy carries the original's
        // `instance.json` for as long as it runs, so an interrupted duplication
        // would otherwise leave a second root claiming the original's identity.
        metadata::mark_incomplete(&root)?;

        let skip = |relative: &Path| {
            metadata::is_transaction_state(relative) || crate::lock::is_launch_state(relative)
        };
        let report = crate::copy::copy_tree(&original.root, &root, &skip).map_err(|source| {
            RegistryError::Io {
                path: root.clone(),
                source,
            }
        })?;

        let now = Utc::now();
        let record = ClientRecord {
            id,
            name: request
                .name
                .clone()
                .unwrap_or_else(|| format!("{}{DUPLICATE_NAME_SUFFIX}", original.name)),
            root: root.clone(),
            storage,
            minecraft_version: original.minecraft_version,
            loader: original.loader,
            source: original.source,
            java_override: original.java_override,
            jvm_override: original.jvm_override,
            allow_pack_mod_version_changes: original.allow_pack_mod_version_changes,
            attention: original.attention,
            created_at: now,
            updated_at: now,
            last_launched_at: None,
        };

        // The copy brought the original's `.launcher/instance.json` with it;
        // this overwrites it with the new identity before the index learns of
        // the row, in the order the module documentation requires.
        create_dir_all(&metadata::client_game_dir(&root))?;
        metadata::write_client_metadata(&root, &record)?;
        self.publish_client(&record)?;

        tracing::info!(
            instance_id = %record.id,
            duplicated_from = %source,
            storage = %record.storage,
            files = report.files,
            skipped_links = report.skipped_links.len(),
            "duplicated client instance"
        );
        for link in &report.skipped_links {
            tracing::warn!(
                instance_id = %record.id,
                link = %link.display(),
                "did not follow a symlinked directory while duplicating"
            );
        }

        Ok(DuplicatedClient {
            record,
            skipped_links: report.skipped_links,
        })
    }

    /// Reads one client instance, addressed where its files are.
    ///
    /// A row that still names a vacated temporary root while the finished
    /// files stand at the persistent root, which a move whose index write
    /// failed leaves behind, is returned at the persistent root. Launching,
    /// duplicating, mutating, and deleting the instance then act on the files
    /// that exist (ADR 0002, ADR 0025). Reconciliation reads the row as the
    /// index holds it, through [`Self::indexed_client`].
    pub fn client(&self, id: InstanceId) -> Result<ClientRecord, RegistryError> {
        Ok(self.following_moved_files(self.indexed_client(id)?))
    }

    /// One client instance exactly as the index row holds it.
    ///
    /// For code that repairs or retries what the row says, such as
    /// reconciliation and a repeated move. Everything that acts on the
    /// instance's files reads [`Self::client`].
    pub fn indexed_client(&self, id: InstanceId) -> Result<ClientRecord, RegistryError> {
        self.index
            .connection()
            .query_row(
                &format!("SELECT {CLIENT_COLUMNS} FROM instances WHERE id = ?1"),
                params![id.to_string()],
                client_from_row,
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => RegistryError::InstanceUnknown { id },
                other => RegistryError::from(other),
            })?
            .map_err(RegistryError::from)
    }

    /// Lists every indexed client instance, newest first.
    pub fn list_clients(&self) -> Result<Vec<ClientRecord>, RegistryError> {
        let connection = self.index.connection();
        let mut statement = connection.prepare(&format!(
            "SELECT {CLIENT_COLUMNS} FROM instances ORDER BY created_at DESC, id"
        ))?;
        let rows = statement.query_map([], client_from_row)?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row??);
        }
        Ok(records)
    }

    /// Persists changes to a client instance.
    ///
    /// A metadata directory sync failure, or an index failure after the
    /// metadata was published, is logged and does not fail the update;
    /// [`Self::update_client_committed`] reports it to the caller.
    pub fn update_client(&self, record: &mut ClientRecord) -> Result<(), RegistryError> {
        let _warnings = self.update_client_committed(record)?;
        Ok(())
    }

    /// Persists changes to a client instance and reports a published but
    /// unsynced metadata file, or an index mirror that failed after
    /// publication, as a warning on the committed update.
    ///
    /// `updated_at` is stamped here rather than by the caller so it cannot be
    /// forgotten. The metadata file is refreshed only when the root still
    /// exists - a persistent instance whose directory vanished keeps its index
    /// row and its warning, and must not fail the update that records that
    /// warning.
    pub fn update_client_committed(
        &self,
        record: &mut ClientRecord,
    ) -> Result<Vec<ClientCommitWarning>, RegistryError> {
        record.updated_at = Utc::now();

        // An instance the index does not list, such as one that is not yet
        // published, must be refused before anything is written.
        if !self.is_indexed(RecordRef::Client(record.id))? {
            return Err(RegistryError::InstanceUnknown { id: record.id });
        }

        // A directory sync failure after the rename leaves the new metadata
        // visible, so the mutation is committed and the index must follow it.
        let mut warnings = Vec::new();
        let published = record.root.is_dir();
        if published {
            if let AtomicPublication::Unsynced(error) =
                metadata::publish_client_metadata(&record.root, record)?
            {
                tracing::warn!(instance_id = %record.id, %error, "client metadata was published but its directory was not synced");
                warnings.push(ClientCommitWarning::MetadataNotDurable {
                    detail: error.to_string(),
                });
            }
        } else {
            tracing::debug!(
                instance_id = %record.id,
                root = %record.root.display(),
                "instance root is missing; updating the index only"
            );
        }

        // Once `instance.json` is published the mutation is committed, so a
        // failing index mirror is a warning. Without a published file the
        // index is the only record and its failure is the update's failure.
        if let Err(error) = self.update_client_row(record) {
            if !published {
                return Err(error);
            }
            tracing::warn!(instance_id = %record.id, %error, "client metadata was published but the index could not be updated");
            warnings.push(ClientCommitWarning::IndexOutOfSync {
                detail: error.to_string(),
            });
        }
        Ok(warnings)
    }

    /// Applies one change to a client's current record and persists it.
    ///
    /// The record is read here, immediately before the write, from the
    /// canonical metadata when the root exists, because the index row lags it
    /// after an index write failed behind a committed update (ADR 0002). Only
    /// the fields `change` touches differ from what is stored. Operations that
    /// spend minutes downloading must use this instead of writing back the
    /// record they read when they started: a rename, a setting, or an
    /// attention flag stored meanwhile would be reverted.
    ///
    /// Metadata that is unreadable or names another instance refuses the
    /// change instead of being replaced by the index copy.
    pub fn modify_client(
        &self,
        id: InstanceId,
        change: impl FnOnce(&mut ClientRecord),
    ) -> Result<ClientRecord, RegistryError> {
        Ok(self.modify_client_committed(id, change)?.record)
    }

    /// [`Self::modify_client`] that also reports a published but unsynced
    /// metadata file as a warning on the committed change.
    pub fn modify_client_committed(
        &self,
        id: InstanceId,
        change: impl FnOnce(&mut ClientRecord),
    ) -> Result<ClientCommit<ClientRecord>, RegistryError> {
        let mut record = self.current_client(id)?;
        change(&mut record);
        let warnings = self.update_client_committed(&mut record)?;
        Ok(ClientCommit { record, warnings })
    }

    /// The client as its canonical metadata holds it, at the root the index
    /// names, or at the persistent root its files were moved to when the row
    /// still names the vacated temporary one. A client whose root is missing
    /// has only its index row.
    ///
    /// Anything that plans from a client's current Minecraft version, loader,
    /// or overrides reads this instead of [`Self::client`], whose row lags the
    /// metadata after an index write failed behind a committed update
    /// (ADR 0002). Metadata that is unreadable or names another instance is an
    /// error, never replaced by the index copy.
    ///
    /// The exception is a row that recorded changes while the root was
    /// missing: only the index could hold them, so that newer row is current
    /// once the root returns, as reconciliation adopts it.
    pub fn current_client(&self, id: InstanceId) -> Result<ClientRecord, RegistryError> {
        let indexed = self.client(id)?;
        if !indexed.root.is_dir() {
            return Ok(indexed);
        }
        let mut canonical = canonical_client_at(&indexed.root, id)?;
        if client_index_is_newer(&indexed, &canonical) {
            let mut returned = indexed;
            returned
                .attention
                .retain(|reason| reason != &AttentionReason::MissingRoot);
            return Ok(returned);
        }
        canonical.root = indexed.root;
        canonical.storage = indexed.storage;
        Ok(canonical)
    }

    /// The record addressed at the persistent root when its row still names a
    /// vacated temporary one and the finished files stand there. A move whose
    /// index write failed leaves exactly this, and a change applied to the row
    /// alone would never reach the metadata (ADR 0002, ADR 0025). The next
    /// write then repairs the row.
    fn following_moved_files(&self, indexed: ClientRecord) -> ClientRecord {
        if indexed.root.is_dir() || !indexed.storage.vanishing_is_expected() {
            return indexed;
        }
        let moved = self
            .paths
            .instance_root(StorageClass::Persistent, indexed.id);
        if !crate::reconcile::is_finished_client_root(&moved, indexed.id) {
            return indexed;
        }
        ClientRecord {
            root: moved,
            storage: StorageClass::Persistent,
            ..indexed
        }
    }

    /// Replaces the SQLite recovery copy of an indexed client with its
    /// canonical metadata, exactly as read.
    ///
    /// Unlike [`Self::update_client`] this neither stamps `updated_at` nor
    /// rewrites the metadata file, so mirroring never changes the record it
    /// mirrors. The row is updated in place: replacing it would unlink the
    /// servers that reference it.
    pub fn synchronize_client_index(&self, record: &ClientRecord) -> Result<(), RegistryError> {
        self.update_client_row(record)
    }

    fn update_client_row(&self, record: &ClientRecord) -> Result<(), RegistryError> {
        let changed = self.index.connection().execute(
            "UPDATE instances SET
                name = ?2, root = ?3, storage_class = ?4, minecraft_version = ?5,
                loader_kind = ?6, loader_version = ?7, source_kind = ?8, source_reference = ?9,
                java_override = ?10, jvm_override = ?11, allow_pack_mod_version_changes = ?12,
                attention = ?13, updated_at = ?14, last_launched_at = ?15
             WHERE id = ?1",
            params![
                record.id.to_string(),
                record.name,
                path_to_sql(&record.root),
                storage_class_to_sql(record.storage),
                record.minecraft_version,
                loader_kind_to_sql(record.loader.kind),
                record.loader.version,
                source_kind_to_sql(record.source.kind),
                record.source.reference,
                to_json_option(record.java_override.as_ref())?,
                to_json_option(record.jvm_override.as_ref())?,
                record.allow_pack_mod_version_changes,
                to_json(&record.attention)?,
                timestamp_to_sql(record.updated_at),
                record.last_launched_at.map(timestamp_to_sql),
            ],
        )?;

        if changed == 0 {
            return Err(RegistryError::InstanceUnknown { id: record.id });
        }
        Ok(())
    }

    /// Removes a client instance from the index, and optionally from disk.
    ///
    /// Deleting files is never inferred: [`FileDisposition::Keep`] is what
    /// reconciliation uses when the directory is already gone, and what
    /// "forget this instance" uses when the user wants to keep the files
    /// (ADR 0006).
    ///
    /// Removing files is refused when the root holds another instance's
    /// metadata, because the directory is not verifiably this instance's.
    /// Forgetting the record does not read that metadata.
    pub fn delete_client(
        &self,
        id: InstanceId,
        files: FileDisposition,
    ) -> Result<(), RegistryError> {
        let record = self.client(id)?;
        self.refuse_removing_foreign_client_files(id, files)?;

        // The relationship is owned canonically by each server. Clear every
        // metadata record before SQLite's ON DELETE SET NULL mirrors it.
        // Every record is read and checked before any is written, so a server
        // whose metadata is unreadable or names another server refuses the
        // deletion without having changed the others.
        let mut linked = Vec::new();
        for server_id in self.linked_server_ids(id)? {
            let indexed = self.server(server_id)?;
            let server = if indexed.root.is_dir() {
                let mut canonical = canonical_server_at(&indexed.root, server_id)?;
                canonical.root = indexed.root;
                canonical.storage = indexed.storage;
                canonical
            } else {
                indexed
            };
            linked.push(server);
        }
        for mut server in linked {
            server.linked_client_id = None;
            let _committed = self.update_server_committed(&mut server)?;
        }

        if files == FileDisposition::Remove {
            remove_tree(&record.root)?;
        }

        self.index.connection().execute(
            "DELETE FROM instances WHERE id = ?1",
            params![id.to_string()],
        )?;

        self.discard_host_guards(RecordRef::Client(id));

        tracing::info!(instance_id = %id, ?files, "removed client instance from the index");
        Ok(())
    }

    /// Refuses removing the files of a client whose root holds another
    /// instance's metadata. Callers that delete other records first run this
    /// before the first change, so a refusal removes nothing.
    pub fn refuse_removing_foreign_client_files(
        &self,
        id: InstanceId,
        files: FileDisposition,
    ) -> Result<(), RegistryError> {
        let record = self.client(id)?;
        if files == FileDisposition::Remove
            && metadata::read_client_metadata(&record.root).is_ok_and(|found| found.id != id)
        {
            return Err(RegistryError::InstanceMetadataForeign { path: record.root });
        }
        Ok(())
    }

    /// The servers linked to a client instance.
    ///
    /// Derived from the servers table rather than stored on the client, so it
    /// cannot go stale (see [`ClientRecord`]).
    pub fn linked_server_ids(&self, client: InstanceId) -> Result<Vec<ServerId>, RegistryError> {
        let connection = self.index.connection();
        let mut statement = connection
            .prepare("SELECT id FROM servers WHERE linked_client_id = ?1 ORDER BY name")?;
        let rows =
            statement.query_map(params![client.to_string()], |row| row.get::<_, String>(0))?;

        let mut ids = Vec::new();
        for row in rows {
            let raw = row?;
            ids.push(parse_id(&raw, "servers.id").map_err(RegistryError::from)?);
        }
        Ok(ids)
    }

    // ---------------------------------------------------------------- servers

    /// Creates a server: directory, metadata, then index row.
    pub fn create_server(&self, request: NewServer) -> Result<ServerRecord, RegistryError> {
        let record = self.prepare_server(request)?;
        create_dir_all(&metadata::server_game_dir(&record.root))?;
        metadata::write_server_metadata(&record.root, &record)?;
        self.insert_server_row(&record)?;

        tracing::info!(
            server_id = %record.id,
            storage = %record.storage,
            root = %record.root.display(),
            "created server"
        );
        Ok(record)
    }

    /// Resolves a request into its canonical record without publishing files.
    /// Linked requests derive their exact selection from the client here.
    pub fn prepare_server(&self, request: NewServer) -> Result<ServerRecord, RegistryError> {
        let id = ServerId::new();
        let root = self.paths.server_root(request.storage, id);
        ensure_free_root(&root)?;

        let (linked_client_id, minecraft_version, loader) = match request.creation {
            ServerCreationSource::Standalone {
                minecraft_version,
                loader,
            } => (None, minecraft_version, loader),
            ServerCreationSource::LinkedClient { client_id } => {
                let client = self.current_client(client_id)?;
                (Some(client_id), client.minecraft_version, client.loader)
            }
        };

        let now = Utc::now();
        Ok(ServerRecord {
            id,
            name: request.name,
            root: root.clone(),
            storage: request.storage,
            linked_client_id,
            minecraft_version,
            loader,
            installation: ServerInstallation::NotInstalled,
            network: request.network,
            operator_setup: None,
            source: request.source,
            java_override: None,
            jvm_override: None,
            attention: request.attention,
            created_at: now,
            updated_at: now,
            last_started_at: None,
        })
    }

    /// Publishes a complete staging root, then mirrors it into SQLite.
    ///
    /// The root rename is the commit point. An index failure after it is a
    /// warning attached to the committed record, never a failed creation.
    pub fn publish_staged_server(
        &self,
        record: &ServerRecord,
        staging_root: &Path,
    ) -> Result<ServerCommit<ServerRecord>, RegistryError> {
        ensure_free_root(&record.root)?;
        metadata::write_server_metadata(staging_root, record)?;
        std::fs::rename(staging_root, &record.root).map_err(|source| RegistryError::Io {
            path: record.root.clone(),
            source,
        })?;
        let warnings = match self.insert_server_row(record) {
            Ok(()) => Vec::new(),
            Err(error) => vec![ServerCommitWarning::IndexOutOfSync {
                detail: error.to_string(),
            }],
        };
        Ok(ServerCommit {
            record: record.clone(),
            warnings,
        })
    }

    /// Adds an already-existing server directory to the index.
    pub fn index_existing_server(&self, record: &ServerRecord) -> Result<(), RegistryError> {
        self.insert_server_row(record)
    }

    /// Atomically replaces the SQLite recovery copy from canonical metadata.
    pub fn synchronize_server_index(&self, record: &ServerRecord) -> Result<(), RegistryError> {
        self.index.connection().execute(
            "INSERT OR REPLACE INTO servers
             (id, name, root, storage_class, linked_client_id, minecraft_version, loader_kind,
              loader_version, network, eula_accepted, source_kind, source_reference,
              java_override, jvm_override, attention, created_at, updated_at, last_started_at,
              installation, operator_setup)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                record.id.to_string(),
                record.name,
                path_to_sql(&record.root),
                storage_class_to_sql(record.storage),
                record.linked_client_id.map(|id| id.to_string()),
                record.minecraft_version,
                loader_kind_to_sql(record.loader.kind),
                record.loader.version,
                to_json(&record.network)?,
                source_kind_to_sql(record.source.kind),
                record.source.reference,
                to_json_option(record.java_override.as_ref())?,
                to_json_option(record.jvm_override.as_ref())?,
                to_json(&record.attention)?,
                timestamp_to_sql(record.created_at),
                timestamp_to_sql(record.updated_at),
                record.last_started_at.map(timestamp_to_sql),
                to_json(&record.installation)?,
                to_json_option(record.operator_setup.as_ref())?,
            ],
        )?;
        Ok(())
    }

    /// Reads one server.
    pub fn server(&self, id: ServerId) -> Result<ServerRecord, RegistryError> {
        self.index
            .connection()
            .query_row(
                &format!("SELECT {SERVER_COLUMNS} FROM servers WHERE id = ?1"),
                params![id.to_string()],
                server_from_row,
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => RegistryError::ServerUnknown { id },
                other => RegistryError::from(other),
            })?
            .map_err(RegistryError::from)
    }

    /// Lists every indexed server, newest first.
    pub fn list_servers(&self) -> Result<Vec<ServerRecord>, RegistryError> {
        let connection = self.index.connection();
        let mut statement = connection.prepare(&format!(
            "SELECT {SERVER_COLUMNS} FROM servers ORDER BY created_at DESC, id"
        ))?;
        let rows = statement.query_map([], server_from_row)?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row??);
        }
        Ok(records)
    }

    /// Persists changes to a server.
    pub fn update_server(&self, record: &mut ServerRecord) -> Result<(), RegistryError> {
        let _committed = self.update_server_committed(record)?;
        Ok(())
    }

    /// Commits canonical metadata first and reports an index-mirror failure as
    /// a warning rather than rolling back an already durable mutation.
    pub fn update_server_committed(
        &self,
        record: &mut ServerRecord,
    ) -> Result<ServerCommit<ServerRecord>, RegistryError> {
        record.updated_at = Utc::now();

        // A directory sync failure after the rename leaves the new metadata
        // visible, so the mutation is committed and the index must follow it.
        let mut warnings = Vec::new();
        if record.root.is_dir() {
            if let AtomicPublication::Unsynced(error) =
                metadata::publish_server_metadata(&record.root, record)?
            {
                tracing::warn!(server_id = %record.id, %error, "server metadata was published but its directory was not synced");
                warnings.push(ServerCommitWarning::MetadataNotDurable {
                    detail: error.to_string(),
                });
            }
        }

        let indexed = self.index.connection().execute(
            "UPDATE servers SET
                name = ?2, root = ?3, storage_class = ?4, linked_client_id = ?5,
                minecraft_version = ?6, loader_kind = ?7, loader_version = ?8, network = ?9,
                source_kind = ?10, source_reference = ?11,
                java_override = ?12, jvm_override = ?13, attention = ?14,
                updated_at = ?15, last_started_at = ?16, installation = ?17,
                operator_setup = ?18
             WHERE id = ?1",
            params![
                record.id.to_string(),
                record.name,
                path_to_sql(&record.root),
                storage_class_to_sql(record.storage),
                record.linked_client_id.map(|id| id.to_string()),
                record.minecraft_version,
                loader_kind_to_sql(record.loader.kind),
                record.loader.version,
                to_json(&record.network)?,
                source_kind_to_sql(record.source.kind),
                record.source.reference,
                to_json_option(record.java_override.as_ref())?,
                to_json_option(record.jvm_override.as_ref())?,
                to_json(&record.attention)?,
                timestamp_to_sql(record.updated_at),
                record.last_started_at.map(timestamp_to_sql),
                to_json(&record.installation)?,
                to_json_option(record.operator_setup.as_ref())?,
            ],
        );

        match indexed {
            Ok(0) => warnings.push(ServerCommitWarning::IndexOutOfSync {
                detail: RegistryError::ServerUnknown { id: record.id }.to_string(),
            }),
            Ok(_) => {}
            Err(error) => warnings.push(ServerCommitWarning::IndexOutOfSync {
                detail: error.to_string(),
            }),
        }
        Ok(ServerCommit {
            record: record.clone(),
            warnings,
        })
    }

    /// Removes a server from the index, and optionally from disk.
    pub fn delete_server(&self, id: ServerId, files: FileDisposition) -> Result<(), RegistryError> {
        let record = self.server(id)?;
        if files == FileDisposition::Remove
            && metadata::read_server_metadata(&record.root).is_ok_and(|found| found.id != id)
        {
            return Err(RegistryError::ServerMetadataForeign { path: record.root });
        }

        if files == FileDisposition::Remove {
            remove_tree(&record.root)?;
        }

        self.index
            .connection()
            .execute("DELETE FROM servers WHERE id = ?1", params![id.to_string()])?;

        self.discard_host_guards(RecordRef::Server(id));

        tracing::info!(server_id = %id, ?files, "removed server from the index");
        Ok(())
    }

    // ------------------------------------------------------------- statements

    fn insert_client_row(&self, record: &ClientRecord) -> Result<(), RegistryError> {
        self.index.connection().execute(
            "INSERT INTO instances
             (id, name, root, storage_class, minecraft_version, loader_kind, loader_version,
              source_kind, source_reference, java_override, jvm_override,
              allow_pack_mod_version_changes, attention,
              created_at, updated_at, last_launched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                record.id.to_string(),
                record.name,
                path_to_sql(&record.root),
                storage_class_to_sql(record.storage),
                record.minecraft_version,
                loader_kind_to_sql(record.loader.kind),
                record.loader.version,
                source_kind_to_sql(record.source.kind),
                record.source.reference,
                to_json_option(record.java_override.as_ref())?,
                to_json_option(record.jvm_override.as_ref())?,
                record.allow_pack_mod_version_changes,
                to_json(&record.attention)?,
                timestamp_to_sql(record.created_at),
                timestamp_to_sql(record.updated_at),
                record.last_launched_at.map(timestamp_to_sql),
            ],
        )?;
        Ok(())
    }

    fn insert_server_row(&self, record: &ServerRecord) -> Result<(), RegistryError> {
        self.index.connection().execute(
            "INSERT INTO servers
             (id, name, root, storage_class, linked_client_id, minecraft_version, loader_kind,
              loader_version, network, eula_accepted, source_kind, source_reference,
              java_override, jvm_override, attention, created_at, updated_at, last_started_at,
              installation, operator_setup)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                record.id.to_string(),
                record.name,
                path_to_sql(&record.root),
                storage_class_to_sql(record.storage),
                record.linked_client_id.map(|id| id.to_string()),
                record.minecraft_version,
                loader_kind_to_sql(record.loader.kind),
                record.loader.version,
                to_json(&record.network)?,
                source_kind_to_sql(record.source.kind),
                record.source.reference,
                to_json_option(record.java_override.as_ref())?,
                to_json_option(record.jvm_override.as_ref())?,
                to_json(&record.attention)?,
                timestamp_to_sql(record.created_at),
                timestamp_to_sql(record.updated_at),
                record.last_started_at.map(timestamp_to_sql),
                to_json(&record.installation)?,
                to_json_option(record.operator_setup.as_ref())?,
            ],
        )?;
        Ok(())
    }
}

// ------------------------------------------------------------- row conversion
//
// Rows come back as `Result<Result<Record, IndexError>, rusqlite::Error>`: the
// outer error is SQLite's, the inner one is ours for a row SQLite handed over
// happily but this build cannot interpret - an enum name from a newer version,
// say. Collapsing the two would mean reporting "database error" for what is
// really "this row was written by a different Ephervex".

fn client_from_row(row: &Row<'_>) -> rusqlite::Result<Result<ClientRecord, IndexError>> {
    let id: String = row.get(0)?;
    let storage: String = row.get(3)?;
    let loader_kind: String = row.get(5)?;
    let source_kind: String = row.get(7)?;
    let java_override: Option<String> = row.get(9)?;
    let jvm_override: Option<String> = row.get(10)?;
    let allow_pack_mod_version_changes: bool = row.get(11)?;
    let attention: String = row.get(12)?;
    let created_at: String = row.get(13)?;
    let updated_at: String = row.get(14)?;
    let last_launched_at: Option<String> = row.get(15)?;

    let record = (|| {
        Ok(ClientRecord {
            id: parse_id(&id, "instances.id")?,
            name: row.get(1).unwrap_or_default(),
            root: PathBuf::from(row.get::<_, String>(2).unwrap_or_default()),
            storage: storage_class_from_sql(&storage)?,
            minecraft_version: row.get(4).unwrap_or_default(),
            loader: LoaderSelection {
                kind: loader_kind_from_sql(&loader_kind)?,
                version: row.get(6).unwrap_or_default(),
            },
            source: ImportSource {
                kind: source_kind_from_sql(&source_kind)?,
                reference: row.get(8).unwrap_or_default(),
            },
            java_override: from_json_option::<JavaOverride>(
                java_override.as_deref(),
                "instances.java_override",
            )?,
            jvm_override: from_json_option::<JvmSettings>(
                jvm_override.as_deref(),
                "instances.jvm_override",
            )?,
            allow_pack_mod_version_changes,
            attention: from_json::<Vec<AttentionReason>>(&attention, "instances.attention")?,
            created_at: timestamp_from_sql(&created_at, "instances.created_at")?,
            updated_at: timestamp_from_sql(&updated_at, "instances.updated_at")?,
            last_launched_at: last_launched_at
                .as_deref()
                .map(|value| timestamp_from_sql(value, "instances.last_launched_at"))
                .transpose()?,
        })
    })();

    Ok(record)
}

fn server_from_row(row: &Row<'_>) -> rusqlite::Result<Result<ServerRecord, IndexError>> {
    let id: String = row.get(0)?;
    let storage: String = row.get(3)?;
    let linked_client_id: Option<String> = row.get(4)?;
    let loader_kind: String = row.get(6)?;
    let network: String = row.get(8)?;
    let source_kind: String = row.get(10)?;
    let java_override: Option<String> = row.get(12)?;
    let jvm_override: Option<String> = row.get(13)?;
    let attention: String = row.get(14)?;
    let created_at: String = row.get(15)?;
    let updated_at: String = row.get(16)?;
    let last_started_at: Option<String> = row.get(17)?;
    let installation: String = row.get(18)?;
    let operator_setup: Option<String> = row.get(19)?;

    let record = (|| {
        Ok(ServerRecord {
            id: parse_id(&id, "servers.id")?,
            name: row.get(1).unwrap_or_default(),
            root: PathBuf::from(row.get::<_, String>(2).unwrap_or_default()),
            storage: storage_class_from_sql(&storage)?,
            linked_client_id: linked_client_id
                .as_deref()
                .map(|value| parse_id(value, "servers.linked_client_id"))
                .transpose()?,
            minecraft_version: row.get(5).unwrap_or_default(),
            loader: LoaderSelection {
                kind: loader_kind_from_sql(&loader_kind)?,
                version: row.get(7).unwrap_or_default(),
            },
            installation: from_json::<ServerInstallation>(&installation, "servers.installation")?,
            network: from_json::<ServerNetworkSettings>(&network, "servers.network")?,
            operator_setup: from_json_option(operator_setup.as_deref(), "servers.operator_setup")?,
            source: ImportSource {
                kind: source_kind_from_sql(&source_kind)?,
                reference: row.get(11).unwrap_or_default(),
            },
            java_override: from_json_option::<JavaOverride>(
                java_override.as_deref(),
                "servers.java_override",
            )?,
            jvm_override: from_json_option::<JvmSettings>(
                jvm_override.as_deref(),
                "servers.jvm_override",
            )?,
            attention: from_json::<Vec<AttentionReason>>(&attention, "servers.attention")?,
            created_at: timestamp_from_sql(&created_at, "servers.created_at")?,
            updated_at: timestamp_from_sql(&updated_at, "servers.updated_at")?,
            last_started_at: last_started_at
                .as_deref()
                .map(|value| timestamp_from_sql(value, "servers.last_started_at"))
                .transpose()?,
        })
    })();

    Ok(record)
}

// ------------------------------------------------------------------- encoding
//
// Enums are written as stable lowercase identifiers chosen here rather than via
// `Display`, which renders product names ("NeoForge") for the UI. The two must
// be allowed to diverge: renaming a product must never rewrite a database.

const fn storage_class_to_sql(value: StorageClass) -> &'static str {
    match value {
        StorageClass::Temporary => "temporary",
        StorageClass::Persistent => "persistent",
    }
}

fn storage_class_from_sql(value: &str) -> Result<StorageClass, IndexError> {
    match value {
        "temporary" => Ok(StorageClass::Temporary),
        "persistent" => Ok(StorageClass::Persistent),
        other => Err(IndexError::MalformedRow {
            column: "storage_class",
            value: other.to_owned(),
        }),
    }
}

const fn loader_kind_to_sql(value: LoaderKind) -> &'static str {
    match value {
        LoaderKind::Vanilla => "vanilla",
        LoaderKind::Forge => "forge",
        LoaderKind::NeoForge => "neoforge",
    }
}

fn loader_kind_from_sql(value: &str) -> Result<LoaderKind, IndexError> {
    match value {
        "vanilla" => Ok(LoaderKind::Vanilla),
        "forge" => Ok(LoaderKind::Forge),
        "neoforge" => Ok(LoaderKind::NeoForge),
        other => Err(IndexError::MalformedRow {
            column: "loader_kind",
            value: other.to_owned(),
        }),
    }
}

const fn source_kind_to_sql(value: ImportSourceKind) -> &'static str {
    match value {
        ImportSourceKind::Manual => "manual",
        ImportSourceKind::ModrinthPack => "modrinth_pack",
        ImportSourceKind::CurseForgePack => "curseforge_pack",
        ImportSourceKind::PrismExport => "prism_export",
        ImportSourceKind::ServerPack => "server_pack",
    }
}

fn source_kind_from_sql(value: &str) -> Result<ImportSourceKind, IndexError> {
    match value {
        "manual" => Ok(ImportSourceKind::Manual),
        "modrinth_pack" => Ok(ImportSourceKind::ModrinthPack),
        "curseforge_pack" => Ok(ImportSourceKind::CurseForgePack),
        "prism_export" => Ok(ImportSourceKind::PrismExport),
        "server_pack" => Ok(ImportSourceKind::ServerPack),
        other => Err(IndexError::MalformedRow {
            column: "source_kind",
            value: other.to_owned(),
        }),
    }
}

fn path_to_sql(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn timestamp_to_sql(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}

fn timestamp_from_sql(value: &str, column: &'static str) -> Result<DateTime<Utc>, IndexError> {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| IndexError::MalformedRow {
            column,
            value: value.to_owned(),
        })
}

fn parse_id<T: std::str::FromStr>(value: &str, column: &'static str) -> Result<T, IndexError> {
    value.parse().map_err(|_| IndexError::MalformedRow {
        column,
        value: value.to_owned(),
    })
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, IndexError> {
    serde_json::to_string(value).map_err(|error| IndexError::MalformedRow {
        column: "json",
        value: error.to_string(),
    })
}

fn to_json_option<T: serde::Serialize>(value: Option<&T>) -> Result<Option<String>, IndexError> {
    value.map(to_json).transpose()
}

fn from_json<T: serde::de::DeserializeOwned>(
    value: &str,
    column: &'static str,
) -> Result<T, IndexError> {
    serde_json::from_str(value).map_err(|_| IndexError::MalformedRow {
        column,
        value: value.to_owned(),
    })
}

fn from_json_option<T: serde::de::DeserializeOwned>(
    value: Option<&str>,
    column: &'static str,
) -> Result<Option<T>, IndexError> {
    value.map(|raw| from_json(raw, column)).transpose()
}

/// Refuses to build an instance on top of somebody else's files.
///
/// An id collision is astronomically unlikely; a stale directory left by a
/// previous run at the same id is not, and silently merging into it would
/// produce an instance that reproduces the wrong thing.
fn ensure_free_root(root: &Path) -> Result<(), RegistryError> {
    // Every root checked here is about to be recorded in metadata, which holds
    // text only.
    crate::paths::utf8_path(root)?;
    match std::fs::read_dir(root) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                Err(RegistryError::RootOccupied {
                    path: root.to_path_buf(),
                })
            } else {
                Ok(())
            }
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(RegistryError::Io {
            path: root.to_path_buf(),
            source,
        }),
    }
}

/// A client's record as its canonical metadata at `root` holds it, refusing
/// metadata that is unreadable or names another instance (ADR 0002). Foreign
/// metadata is its own failure, so the user is told which directory holds it.
///
/// The stored root and storage class are whatever the file last said; the
/// caller sets the ones it is changing.
pub(crate) fn canonical_client_at(
    root: &Path,
    id: InstanceId,
) -> Result<ClientRecord, RegistryError> {
    let found = metadata::read_client_metadata(root)?;
    if found.id != id {
        return Err(RegistryError::InstanceMetadataForeign {
            path: root.to_path_buf(),
        });
    }
    Ok(found)
}

/// Whether an index row holds a later state than the canonical metadata.
///
/// Only a row flagged `MissingRoot` can: while the root was absent an update
/// could reach the index alone.
pub(crate) fn client_index_is_newer(row: &ClientRecord, canonical: &ClientRecord) -> bool {
    row.attention.contains(&AttentionReason::MissingRoot) && row.updated_at > canonical.updated_at
}

/// The same for a dedicated server (ADR 0014).
pub(crate) fn canonical_server_at(
    root: &Path,
    id: ServerId,
) -> Result<ServerRecord, RegistryError> {
    let found = metadata::read_server_metadata(root)?;
    if found.id != id {
        return Err(RegistryError::ServerMetadataForeign {
            path: root.to_path_buf(),
        });
    }
    Ok(found)
}

/// Where a host's current files stand when a move records itself.
///
/// A copy leaves the source in place until the move is recorded, and edits
/// stored while it ran went there. A rename, a recovered rename, and a record
/// that already named the destination have no source left.
fn host_files_root<'a>(source: &'a Path, destination: &'a Path) -> &'a Path {
    if source != destination && source.is_dir() {
        source
    } else {
        destination
    }
}

/// Refuses a move whose source metadata could not be recorded afterwards, so
/// the refusal comes before a long copy rather than after it.
fn refuse_unhealthy_source(
    source: &Path,
    destination: &Path,
    read: impl FnOnce(&Path) -> Result<(), RegistryError>,
) -> Result<(), RegistryError> {
    if source != destination && source.is_dir() {
        read(source)?;
    }
    Ok(())
}

/// The move both host kinds share: move the tree, record the move, then remove
/// the old copy (ADR 0025).
///
/// `quiesce` runs on the source just before its files move, and refuses the
/// move when something still holds the host. `record` writes the moved host's
/// metadata and index row. A copy it fails to record is withdrawn; a renamed
/// tree is the only copy and is kept. The caller holds the host's
/// [`HostClaim`].
fn move_host<T>(
    source: &Path,
    destination: &Path,
    names_this_host: impl Fn(&Path) -> bool,
    quiesce: impl FnOnce(&Path) -> Result<(), crate::lock::LockError>,
    progress: &mut dyn FnMut(usize),
    record: impl FnOnce() -> Result<T, RegistryError>,
) -> Result<(T, crate::relocate::RelocationReport), RegistryError> {
    if !source.is_dir() {
        // Nothing else is moving this host, and a copy only reaches the
        // destination complete, so a tree standing there under this host's
        // name is one an earlier move renamed but could not record.
        if names_this_host(destination) {
            let renamed = crate::relocate::RelocationReport {
                renamed: true,
                ..crate::relocate::RelocationReport::default()
            };
            return Ok((record()?, renamed));
        }
        return Err(RegistryError::RootMissing {
            path: source.to_path_buf(),
        });
    }

    if source == destination {
        // The record names a root the host already stands at, so only the
        // record is stale.
        let renamed = crate::relocate::RelocationReport {
            renamed: true,
            ..crate::relocate::RelocationReport::default()
        };
        return Ok((record()?, renamed));
    }

    // The marker of a published host is a snapshot restore's. Recovery would
    // put the host's pre-restore metadata back over the move's, so the move
    // waits for startup to recover it.
    if metadata::is_incomplete(source) {
        return Err(RegistryError::RestoreUnfinished {
            path: source.to_path_buf(),
        });
    }

    // The source still stands, so it is the host. A tree at the destination
    // under this host's name is a copy an earlier move finished but could not
    // record, and the caller's claim means no live move owns it.
    if names_this_host(destination) {
        tracing::info!(
            path = %destination.display(),
            "replacing a finished copy that an earlier move could not record"
        );
        remove_tree(destination)?;
    }
    ensure_free_root(destination)?;
    quiesce(source)?;
    let report =
        crate::relocate::relocate_tree(source, destination, progress).map_err(|error| {
            RegistryError::Io {
                path: destination.to_path_buf(),
                source: error,
            }
        })?;

    // Metadata first, then the index, so an interruption leaves a directory
    // reconciliation can adopt rather than a row pointing at nothing.
    let recorded = record().inspect_err(|_| {
        if !report.renamed
            && let Err(error) = crate::relocate::discard_tree(destination)
        {
            tracing::warn!(
                path = %destination.display(),
                %error,
                "could not remove the copy of a move that could not be recorded"
            );
        }
    })?;

    // What is left at the old address is a stale copy under a root the OS
    // owns. Failing to remove it costs temporary space and nothing else.
    if !report.renamed
        && let Err(error) = crate::relocate::discard_tree(source)
    {
        tracing::warn!(
            root = %source.display(),
            %error,
            "could not remove a host's old directory after moving it"
        );
    }
    Ok((recorded, report))
}

/// One host's exclusive claim on being changed or started.
///
/// Held for the whole change, so a second restore or move of the host is
/// refused instead of publishing into, copying from, or discarding what the
/// first one is still using, and so reconciliation can tell a change still
/// running from one that was abandoned. A change that needs the host stopped
/// takes the claim before it looks at whether the host is running. Starting a
/// host takes the same claim while it records that it is running, so a start
/// cannot begin between that look and the change. The kernel drops the claim
/// when its holder dies, which is what makes an abandoned change recoverable.
pub struct HostClaim {
    host: RecordRef,
    _guard: FileGuard,
}

impl HostClaim {
    /// Refuses a change to `host` under a claim that covers another.
    fn require(&self, host: RecordRef, root: &Path) -> Result<(), RegistryError> {
        if self.host == host {
            Ok(())
        } else {
            Err(RegistryError::HostNotClaimed {
                path: root.to_path_buf(),
            })
        }
    }
}

impl std::fmt::Debug for HostClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostClaim").finish_non_exhaustive()
    }
}

/// One host's exclusive right to rewrite its recipe, in every launcher process.
///
/// A recipe is changed by reading it, working, and writing the whole of it
/// back, so two launchers doing that at once lose whichever wrote first. The
/// kernel drops the claim when its holder dies.
pub struct RecipeClaim {
    _guard: FileGuard,
}

impl std::fmt::Debug for RecipeClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecipeClaim").finish_non_exhaustive()
    }
}

/// What reconciliation learns by probing a host's claim.
pub(crate) enum HostProbe {
    /// Nothing is changing the host; the claim is held until it is dropped.
    Idle(#[allow(dead_code)] HostClaim),
    /// A change or start of the host, in this process or another, is running.
    Mutating,
    /// The claim could not be tried, so an abandoned change cannot be told
    /// from a running one.
    Unusable,
}

impl Registry {
    /// Claims an indexed host for a change that must not overlap another change
    /// or a start of it, refusing while another holds it.
    pub fn claim_host(&self, host: RecordRef) -> Result<HostClaim, RegistryError> {
        let root = match host {
            RecordRef::Client(id) => self.client(id)?.root,
            RecordRef::Server(id) => self.server(id)?.root,
        };
        claim_host(&self.paths, host, &root)
    }

    /// Tries a host's recipe claim without waiting: `None` while a thread or
    /// process holds it.
    ///
    /// Separate from [`Self::claim_host`], so a recipe change neither refuses
    /// nor is refused by a restore or move that only needs the tree to stay put.
    pub fn try_claim_recipe(&self, host: RecordRef) -> Result<Option<RecipeClaim>, RegistryError> {
        match try_take_guard(&self.paths, &recipe_guard_name(host)) {
            Ok(guard) => Ok(Some(RecipeClaim { _guard: guard })),
            Err(GuardError::Busy { .. }) => Ok(None),
            Err(GuardError::Unusable { path, source }) => {
                Err(RegistryError::HostGuardUnusable { path, source })
            }
        }
    }

    /// Tries a host's claim without waiting.
    pub(crate) fn probe_host(&self, host: RecordRef) -> HostProbe {
        match try_claim_host(&self.paths, host) {
            Ok(guard) => HostProbe::Idle(HostClaim {
                host,
                _guard: guard,
            }),
            Err(GuardError::Busy { .. }) => HostProbe::Mutating,
            Err(GuardError::Unusable { path, source }) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %source,
                    "could not tell whether a host is being changed"
                );
                HostProbe::Unusable
            }
        }
    }
}

impl Registry {
    /// Removes a deleted host's claim files, so the directory they live in
    /// does not grow with every host that ever existed.
    ///
    /// A claim a change still holds is removed as the change releases it. The
    /// host is already gone, so a failure here costs a stray file and no more.
    fn discard_host_guards(&self, host: RecordRef) {
        let directory = self.paths.move_guards_dir();
        for name in [host_guard_name(host), recipe_guard_name(host)] {
            if let Err(error) = discard_guard(&directory, &name) {
                log_guard_failure(&error, "could not remove a deleted host's claim file");
            }
        }
    }

    /// Removes the claim files of hosts the index does not list, left by
    /// deletions that could not remove them and by hosts that never got a
    /// record.
    ///
    /// A claim another process holds is left for a later sweep, which keeps a
    /// host being created or adopted from losing the claim it already has.
    pub(crate) fn prune_unknown_host_guards(&self) {
        let directory = self.paths.move_guards_dir();
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                tracing::warn!(
                    path = %directory.display(),
                    %error,
                    "could not list the host claim files"
                );
                return;
            }
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let Some(host) = host_of_guard_name(name) else {
                continue;
            };
            match self.is_indexed(host) {
                Ok(true) => {}
                Ok(false) => {
                    if let Err(error) = discard_guard(&directory, name) {
                        log_guard_failure(&error, "could not remove an unknown host's claim file");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "could not tell whether a host claim file is in use");
                    return;
                }
            }
        }
    }

    fn is_indexed(&self, host: RecordRef) -> Result<bool, RegistryError> {
        let (sql, id) = match host {
            RecordRef::Client(id) => ("SELECT 1 FROM instances WHERE id = ?1", id.to_string()),
            RecordRef::Server(id) => ("SELECT 1 FROM servers WHERE id = ?1", id.to_string()),
        };
        let connection = self.index.connection();
        let mut statement = connection.prepare(sql)?;
        Ok(statement.exists(params![id])?)
    }
}

fn log_guard_failure(error: &GuardError, message: &str) {
    match error {
        GuardError::Busy { path } => {
            tracing::debug!(path = %path.display(), "{message}: it is held");
        }
        GuardError::Unusable { path, source } => {
            tracing::warn!(path = %path.display(), error = %source, "{message}");
        }
    }
}

/// How long taking a host's claim waits before reporting it busy.
///
/// A claim just released can still look held for an instant: a process forked
/// by another thread inherits the lock's file until it execs. That is too short
/// to wait out on purpose and long enough to refuse a launch that nothing else
/// is competing with. A genuine holder keeps the claim far longer than this.
const HOST_CLAIM_PATIENCE: std::time::Duration = std::time::Duration::from_millis(100);

/// Kept outside the tree being changed, so every holder finds it whichever way
/// the tree goes.
fn claim_host(paths: &Paths, host: RecordRef, root: &Path) -> Result<HostClaim, RegistryError> {
    take_guard(paths, &host_guard_name(host), HOST_CLAIM_PATIENCE)
        .map(|guard| HostClaim {
            host,
            _guard: guard,
        })
        .map_err(|error| match error {
            GuardError::Busy { .. } => RegistryError::HostBusy {
                path: root.to_path_buf(),
            },
            GuardError::Unusable { path, source } => {
                RegistryError::HostGuardUnusable { path, source }
            }
        })
}

fn try_claim_host(paths: &Paths, host: RecordRef) -> Result<FileGuard, GuardError> {
    try_take_guard(paths, &host_guard_name(host))
}

fn try_take_guard(paths: &Paths, name: &str) -> Result<FileGuard, GuardError> {
    take_guard(paths, name, std::time::Duration::ZERO)
}

fn take_guard(
    paths: &Paths,
    name: &str,
    patience: std::time::Duration,
) -> Result<FileGuard, GuardError> {
    let directory = paths.move_guards_dir();
    std::fs::create_dir_all(&directory).map_err(|source| GuardError::Unusable {
        path: directory.clone(),
        source,
    })?;
    FileGuard::take(&directory, name, patience)
}

pub(crate) fn host_guard_name(host: RecordRef) -> String {
    match host {
        RecordRef::Client(id) => format!("client-{id}.guard"),
        RecordRef::Server(id) => format!("server-{id}.guard"),
    }
}

fn recipe_guard_name(host: RecordRef) -> String {
    match host {
        RecordRef::Client(id) => format!("client-{id}.recipe.guard"),
        RecordRef::Server(id) => format!("server-{id}.recipe.guard"),
    }
}

/// The host a claim file belongs to, if it is named like one of ours.
///
/// Only the canonical spelling of an id counts, so a file someone else put in
/// the directory is never mistaken for a claim and removed.
fn host_of_guard_name(name: &str) -> Option<RecordRef> {
    let stem = name
        .strip_suffix(".recipe.guard")
        .or_else(|| name.strip_suffix(".guard"))?;
    if let Some(id) = stem.strip_prefix("client-") {
        let parsed: InstanceId = id.parse().ok()?;
        return (parsed.to_string() == id).then_some(RecordRef::Client(parsed));
    }
    let id = stem.strip_prefix("server-")?;
    let parsed: ServerId = id.parse().ok()?;
    (parsed.to_string() == id).then_some(RecordRef::Server(parsed))
}

/// Removes a directory tree, treating "already gone" as success.
///
/// Reconciliation and user deletion both race with the OS clearing the
/// temporary root, and losing that race is not an error (ADR 0006).
fn remove_tree(root: &Path) -> Result<(), RegistryError> {
    crate::removal::remove_tree_if_present(root).map_err(|source| RegistryError::Io {
        path: root.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use ephervex_model::{LoaderKind, OperatorPermissionLevel};

    use crate::test_support::TestEnvironment;
    use crate::{ServerOperatorIdentity, ServerOperatorProfile, ServerOperatorSetup};

    /// Moves a client to the persistent root under its claim, as a caller must.
    fn move_client(
        registry: &Registry,
        id: InstanceId,
        progress: &mut dyn FnMut(usize),
    ) -> Result<ClientRecord, RegistryError> {
        let claim = registry.claim_host(RecordRef::Client(id))?;
        registry
            .relocate_client(id, &claim, StorageClass::Persistent, progress)
            .map(|committed| committed.record)
    }

    fn move_server(
        registry: &Registry,
        id: ServerId,
        progress: &mut dyn FnMut(usize),
    ) -> Result<ServerCommit<ServerRecord>, RegistryError> {
        let claim = registry.claim_host(RecordRef::Server(id))?;
        registry.relocate_server(id, &claim, StorageClass::Persistent, progress)
    }

    #[cfg(unix)]
    #[test]
    fn instances_cannot_be_created_under_a_root_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt as _;

        let directory = tempfile::tempdir().expect("temp dir");
        let base = directory.path();
        let data = base.join(std::ffi::OsStr::from_bytes(b"data\xff"));
        let paths = Paths::from_roots(
            base.join("config"),
            &data,
            base.join("cache"),
            base.join("state"),
            base.join("temp"),
        );
        paths.ensure_base_directories().expect("directories");
        let registry = Registry::open(paths.clone()).expect("open the registry");

        let client = registry
            .create_client(
                NewClient::new("client", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect_err("a root with no text form cannot be recorded");
        let server = registry
            .prepare_server(
                NewServer::new("server", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect_err("a root with no text form cannot be recorded");

        for error in [client, server] {
            assert!(
                matches!(&error, RegistryError::Path(PathError::NotUtf8 { path }) if path.starts_with(&data)),
                "{error:?}"
            );
            assert_eq!(error.message_id(), "error-paths-not-utf8");
        }
        assert!(registry.list_clients().expect("list").is_empty());
        assert_eq!(
            std::fs::read_dir(paths.persistent_instances_dir())
                .expect("instances dir")
                .count(),
            0,
            "no root may be left behind"
        );
    }

    #[test]
    fn a_created_instance_is_readable_listable_and_on_disk() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();

        let record = registry
            .create_published_client(NewClient::new(
                "Crash repro",
                "1.20.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
            ))
            .expect("create");

        assert_eq!(registry.client(record.id).expect("read"), record);
        assert_eq!(registry.list_clients().expect("list"), vec![record.clone()]);
        assert!(metadata::client_metadata_path(&record.root).is_file());
        assert!(metadata::client_game_dir(&record.root).is_dir());
    }

    #[test]
    fn the_metadata_file_alone_is_enough_to_rebuild_the_record() {
        // ADR 0002: the index is a cache of authority, not the authority.
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let record = registry
            .create_published_client(
                NewClient::new(
                    "Persistent",
                    "1.12.2",
                    LoaderSelection::exact(LoaderKind::Forge, "14.23.5.2859"),
                )
                .with_storage(StorageClass::Persistent),
            )
            .expect("create");

        let from_disk = metadata::read_client_metadata(&record.root).expect("read metadata");
        assert_eq!(from_disk, record);
    }

    #[test]
    fn instances_default_to_the_temporary_root_and_persistent_ones_do_not() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();

        let temporary = registry
            .create_published_client(NewClient::new("temp", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let persistent = registry
            .create_published_client(
                NewClient::new("kept", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");

        assert_eq!(temporary.storage, StorageClass::Temporary);
        assert!(
            temporary
                .root
                .starts_with(registry.paths().temporary_instances_dir())
        );
        assert!(
            persistent
                .root
                .starts_with(registry.paths().persistent_instances_dir())
        );
    }

    #[test]
    fn updates_are_persisted_to_both_the_index_and_the_metadata_file() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let mut record = registry
            .create_published_client(NewClient::new(
                "before",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");

        record.name = "after".to_owned();
        record.add_attention(AttentionReason::BestEffortServerConversion);
        registry.update_client(&mut record).expect("update");

        assert_eq!(registry.client(record.id).expect("read").name, "after");
        assert_eq!(
            metadata::read_client_metadata(&record.root)
                .expect("metadata")
                .attention,
            record.attention
        );
    }

    #[test]
    fn deleting_an_instance_can_keep_or_remove_its_files() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();

        let kept = registry
            .create_published_client(NewClient::new("kept", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        registry
            .delete_client(kept.id, FileDisposition::Keep)
            .expect("forget");
        assert!(kept.root.is_dir(), "files must survive a forget");
        assert!(matches!(
            registry.client(kept.id),
            Err(RegistryError::InstanceUnknown { .. })
        ));

        let removed = registry
            .create_published_client(NewClient::new(
                "removed",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        registry
            .delete_client(removed.id, FileDisposition::Remove)
            .expect("delete");
        assert!(!removed.root.exists());
    }

    /// A client instance with a file in its game directory, for the
    /// duplication tests.
    fn client_with_files(registry: &Registry, name: &str) -> ClientRecord {
        let record = registry
            .create_published_client(
                NewClient::new(
                    name,
                    "1.20.1",
                    LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
                )
                .with_storage(StorageClass::Persistent)
                .with_source(ImportSource::imported(
                    ImportSourceKind::ModrinthPack,
                    "report.mrpack",
                )),
            )
            .expect("create");

        let game = metadata::client_game_dir(&record.root);
        std::fs::write(game.join("options.txt"), "guiScale:3\n").expect("write options");
        create_dir_all(&game.join("mods")).expect("mkdir");
        std::fs::write(game.join("mods/example.jar"), "jar").expect("write mod");
        record
    }

    #[test]
    fn a_duplicate_is_a_new_instance_holding_the_same_files() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let original = client_with_files(&registry, "Original");

        let copy = registry
            .duplicate_client(original.id, &DuplicateClient::default())
            .expect("duplicate")
            .record;

        // A new identity...
        assert_ne!(copy.id, original.id);
        assert_ne!(copy.root, original.root);
        assert_eq!(copy.name, "Original (copy)");
        assert!(copy.last_launched_at.is_none());

        //...reproducing the same thing.
        assert_eq!(copy.minecraft_version, original.minecraft_version);
        assert_eq!(copy.loader, original.loader);
        assert_eq!(copy.source, original.source);
        assert_eq!(copy.storage, original.storage);

        let game = metadata::client_game_dir(&copy.root);
        assert_eq!(
            std::fs::read_to_string(game.join("options.txt")).expect("read"),
            "guiScale:3\n",
            "the copy keeps the options file the original had"
        );
        assert!(game.join("mods/example.jar").is_file());

        // Both are indexed, and the copy is recoverable from its own metadata.
        assert_eq!(registry.list_clients().expect("list").len(), 2);
        assert_eq!(
            metadata::read_client_metadata(&copy.root).expect("metadata"),
            copy,
            "the copied instance.json must describe the copy, not the original"
        );
    }

    #[test]
    fn a_duplicate_may_change_its_name_and_its_storage_class() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let original = client_with_files(&registry, "Original");

        let copy = registry
            .duplicate_client(
                original.id,
                &DuplicateClient::named("Disposable try").with_storage(StorageClass::Temporary),
            )
            .expect("duplicate")
            .record;

        assert_eq!(copy.name, "Disposable try");
        assert_eq!(copy.storage, StorageClass::Temporary);
        assert!(
            copy.root
                .starts_with(registry.paths().temporary_instances_dir())
        );
        assert_eq!(
            registry.client(original.id).expect("read").storage,
            StorageClass::Persistent,
            "the original must not move"
        );
    }

    #[test]
    fn a_duplicate_does_not_inherit_a_launch_in_progress() {
        // The lock file describes a process, and the copy is not it
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let original = client_with_files(&registry, "Original");
        let _lock = crate::lock::LaunchLock::acquire(&original.root).expect("lock");

        let copy = registry
            .duplicate_client(original.id, &DuplicateClient::default())
            .expect("duplicate")
            .record;

        assert!(crate::lock::running_state(&original.root).is_some());
        assert!(
            crate::lock::running_state(&copy.root).is_none(),
            "a copy of a running instance is not running"
        );
    }

    #[test]
    fn a_duplicate_inherits_no_additional_launch_and_cannot_stop_the_originals() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let original = client_with_files(&registry, "Original");
        let _first = crate::lock::LaunchLock::acquire(&original.root).expect("lock");
        let _second =
            crate::lock::LaunchLock::acquire_additional(&original.root, "second").expect("lock");

        let copy = registry
            .duplicate_client(original.id, &DuplicateClient::default())
            .expect("duplicate")
            .record;

        assert!(crate::lock::running_launches(&copy.root).is_empty());
        let launch_files: Vec<_> = std::fs::read_dir(metadata::metadata_dir(&copy.root))
            .expect("metadata dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("launch."))
            .collect();
        assert!(launch_files.is_empty(), "copied {launch_files:?}");
        assert_eq!(
            crate::lock::force_release(&copy.root, Some("second")).expect("force"),
            crate::lock::ForcedStop::default(),
            "the copy has nothing of the original's to stop"
        );
        assert_eq!(crate::lock::running_launches(&original.root).len(), 2);
    }

    fn write_file(path: &Path, contents: &str) {
        create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    #[test]
    fn a_duplicate_keeps_game_folders_named_like_launcher_state() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let original = client_with_files(&registry, "Original");
        let game = metadata::client_game_dir(&original.root);
        write_file(&game.join("saves/snapshots/level.dat"), "world");
        write_file(&game.join("config/snapshots/settings.toml"), "config");
        write_file(&game.join("staging/data.txt"), "staging");
        write_file(&game.join("retired/data.txt"), "retired");
        write_file(&game.join("isolation.json"), "{}");

        let copy = registry
            .duplicate_client(original.id, &DuplicateClient::default())
            .expect("duplicate")
            .record;

        let copied = metadata::client_game_dir(&copy.root);
        for relative in [
            "saves/snapshots/level.dat",
            "config/snapshots/settings.toml",
            "staging/data.txt",
            "retired/data.txt",
            "isolation.json",
        ] {
            assert!(copied.join(relative).is_file(), "missing {relative}");
        }
    }

    #[test]
    fn a_duplicate_carries_no_launcher_transaction_state() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let original = client_with_files(&registry, "Original");
        let launcher = metadata::metadata_dir(&original.root);
        write_file(&launcher.join("snapshots/s1/minecraft/options.txt"), "x");
        write_file(&launcher.join("retired/old.jar"), "jar");
        write_file(&launcher.join("staging/restore-1/restore.json"), "{}");
        write_file(&launcher.join("restore-2/restore.json"), "{}");
        write_file(&launcher.join(metadata::ISOLATION_FILE), "{}");
        write_file(&launcher.join("source/recipe.json"), "{}");

        let copy = registry
            .duplicate_client(original.id, &DuplicateClient::default())
            .expect("duplicate")
            .record;

        let copied = metadata::metadata_dir(&copy.root);
        for relative in [
            "snapshots",
            "retired",
            "staging",
            "restore-2",
            metadata::ISOLATION_FILE,
        ] {
            assert!(!copied.join(relative).exists(), "copied {relative}");
        }
        assert!(
            copied.join("source/recipe.json").is_file(),
            "the recipe describes what the instance is and is carried over"
        );
        assert!(metadata::client_metadata_path(&copy.root).is_file());
    }

    #[test]
    fn duplicating_an_instance_whose_files_are_gone_says_so() {
        // A persistent root that vanished keeps its record, so
        // this has to be distinguishable from "no such instance".
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let original = client_with_files(&registry, "Original");
        std::fs::remove_dir_all(&original.root).expect("remove");

        let error = registry
            .duplicate_client(original.id, &DuplicateClient::default())
            .expect_err("nothing to copy");
        assert!(matches!(error, RegistryError::RootMissing { .. }));
        assert_eq!(error.message_id(), "error-registry-root-missing");
    }

    #[test]
    fn duplicating_a_client_does_not_duplicate_its_servers() {
        // a server is its own entity, with its own world and process.
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let original = client_with_files(&registry, "Original");
        registry
            .create_server(
                NewServer::new("test server", "1.20.1", LoaderSelection::vanilla())
                    .linked_to(original.id),
            )
            .expect("create server");

        let copy = registry
            .duplicate_client(original.id, &DuplicateClient::default())
            .expect("duplicate")
            .record;

        assert_eq!(
            registry
                .linked_server_ids(original.id)
                .expect("links")
                .len(),
            1
        );
        assert!(
            registry
                .linked_server_ids(copy.id)
                .expect("links")
                .is_empty()
        );
        assert_eq!(registry.list_servers().expect("list").len(), 1);
    }

    #[test]
    fn several_servers_may_link_to_one_client() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();

        let client = registry
            .create_published_client(NewClient::new(
                "client",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let first = registry
            .create_server(
                NewServer::new("server a", "1.20.1", LoaderSelection::vanilla())
                    .linked_to(client.id),
            )
            .expect("create server");
        let second = registry
            .create_server(
                NewServer::new("server b", "1.20.1", LoaderSelection::vanilla())
                    .linked_to(client.id),
            )
            .expect("create server");
        registry
            .create_server(NewServer::new(
                "standalone",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create standalone");

        let mut linked = registry.linked_server_ids(client.id).expect("linked");
        linked.sort_unstable();
        let mut expected = vec![first.id, second.id];
        expected.sort_unstable();
        assert_eq!(linked, expected);
        assert_eq!(registry.list_servers().expect("list").len(), 3);
    }

    #[test]
    fn deleting_a_client_clears_links_in_canonical_server_metadata() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new(
                "client",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let server = registry
            .create_server(NewServer::linked("server", client.id))
            .expect("create server");

        registry
            .delete_client(client.id, FileDisposition::Remove)
            .expect("delete client");
        assert_eq!(
            metadata::read_server_metadata(&server.root)
                .expect("metadata")
                .linked_client_id,
            None
        );
        assert_eq!(
            registry
                .server(server.id)
                .expect("indexed")
                .linked_client_id,
            None
        );
    }

    #[test]
    fn a_linked_server_preserves_the_clients_exact_minecraft_and_loader_versions() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let loader = LoaderSelection::exact(LoaderKind::NeoForge, "21.1.172");
        let client = registry
            .create_published_client(NewClient::new("client", "1.21.1", loader.clone()))
            .expect("create client");

        let server = registry
            .prepare_server(NewServer::linked("server", client.id))
            .expect("prepare linked server");
        assert_eq!(server.minecraft_version, "1.21.1");
        assert_eq!(server.loader, loader);
        assert_eq!(server.linked_client_id, Some(client.id));
    }

    #[test]
    fn server_operator_intent_round_trips_through_metadata_and_index() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let mut server = registry
            .create_server(NewServer::new(
                "operator setup",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create server");
        server.operator_setup = Some(ServerOperatorSetup {
            permission_level: OperatorPermissionLevel::from_value(3).expect("level"),
            profile: Some(ServerOperatorProfile {
                id: uuid::Uuid::new_v4(),
                name: "Developer".to_owned(),
                identity: ServerOperatorIdentity::Online,
            }),
        });
        registry.update_server(&mut server).expect("update");

        assert_eq!(registry.server(server.id).expect("indexed"), server);
        assert_eq!(
            metadata::read_server_metadata(&server.root).expect("metadata"),
            server
        );
    }

    #[test]
    fn a_new_server_is_explicitly_not_installed() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new("test", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        assert_eq!(server.installation, ServerInstallation::NotInstalled);
        assert!(!server.network.online_mode);

        let metadata = std::fs::read_to_string(metadata::server_metadata_path(&server.root))
            .expect("read metadata");
        assert!(!metadata.contains("eula_accepted"));
    }

    #[test]
    fn records_survive_a_registry_restart_because_they_are_in_the_database() {
        let environment = TestEnvironment::new();
        let record = {
            let registry = environment.registry();
            registry
                .create_published_client(
                    NewClient::new("kept", "1.20.1", LoaderSelection::vanilla())
                        .with_storage(StorageClass::Persistent),
                )
                .expect("create")
        };

        let reopened = environment.registry();
        assert_eq!(reopened.client(record.id).expect("read"), record);
    }

    #[test]
    fn modifying_a_client_keeps_changes_stored_since_the_caller_read_it() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let stale = registry
            .create_published_client(
                NewClient::new("before", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");

        // Another operation stores changes while a long one is still running
        // with the record it read first.
        let mut edited = stale.clone();
        edited.name = "after".to_owned();
        edited.allow_pack_mod_version_changes = true;
        edited.add_attention(AttentionReason::InterruptedMutation);
        registry
            .update_client(&mut edited)
            .expect("concurrent edit");

        let launched_at = Utc::now();
        let written = registry
            .modify_client(stale.id, |current| {
                current.last_launched_at = Some(launched_at);
            })
            .expect("modify");

        for stored in [
            registry.client(stale.id).expect("index copy"),
            metadata::read_client_metadata(&stale.root).expect("metadata copy"),
        ] {
            assert_eq!(stored.name, "after");
            assert!(stored.allow_pack_mod_version_changes);
            assert!(
                stored
                    .attention
                    .contains(&AttentionReason::InterruptedMutation)
            );
            assert_eq!(stored.last_launched_at, Some(launched_at));
        }
        assert_eq!(written.name, "after");
    }

    #[test]
    fn modifying_an_unknown_client_is_reported_as_unknown() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let error = registry
            .modify_client(InstanceId::new(), |_| {})
            .expect_err("nothing there");
        assert!(matches!(error, RegistryError::InstanceUnknown { .. }));
    }

    #[test]
    fn an_unknown_id_is_reported_as_unknown_rather_than_as_a_database_error() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let error = registry
            .client(InstanceId::new())
            .expect_err("nothing there");
        assert!(matches!(error, RegistryError::InstanceUnknown { .. }));
        assert_eq!(error.message_id(), "error-registry-instance-unknown");
    }

    #[test]
    fn a_row_this_build_cannot_interpret_is_reported_as_such() {
        // A database written by a build that knows a loader we do not must not
        // read back as "database error".
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let record = registry
            .create_published_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");

        registry
            .index()
            .connection()
            .execute(
                "UPDATE instances SET loader_kind = 'fabric' WHERE id = ?1",
                params![record.id.to_string()],
            )
            .expect("tamper");

        let error = registry.client(record.id).expect_err("must fail");
        assert!(matches!(
            error,
            RegistryError::Index(IndexError::MalformedRow {
                column: "loader_kind",
                ..
            })
        ));
    }

    #[test]
    fn enum_encodings_are_stable_and_independent_of_display_names() {
        // `LoaderKind::NeoForge` displays as the product name "NeoForge"; the
        // database must not depend on that.
        assert_eq!(loader_kind_to_sql(LoaderKind::NeoForge), "neoforge");
        assert_eq!(
            loader_kind_from_sql("neoforge").unwrap(),
            LoaderKind::NeoForge
        );
        assert_eq!(storage_class_to_sql(StorageClass::Temporary), "temporary");
        assert_eq!(
            source_kind_to_sql(ImportSourceKind::CurseForgePack),
            "curseforge_pack"
        );
        assert!(loader_kind_from_sql("fabric").is_err());
    }

    fn write_payload(directory: &Path) {
        for name in ["one.jar", "two.jar", "three.jar"] {
            std::fs::write(directory.join(name), name).expect("payload");
        }
    }

    fn assert_payload(directory: &Path) {
        for name in ["one.jar", "two.jar", "three.jar"] {
            assert!(directory.join(name).is_file(), "{name} arrived");
        }
    }

    #[test]
    fn a_promotion_across_filesystems_records_the_copy_and_removes_the_source() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        write_payload(&metadata::client_game_dir(&client.root));

        crate::relocate::force_copy(true);
        let promoted = move_client(&registry, client.id, &mut |_| {}).expect("move");
        crate::relocate::force_copy(false);

        assert_eq!(promoted.storage, StorageClass::Persistent);
        assert_eq!(
            registry.client(client.id).expect("indexed").root,
            promoted.root
        );
        assert_payload(&metadata::client_game_dir(&promoted.root));
        assert!(
            !client.root.exists(),
            "the old copy is removed once recorded"
        );
    }

    #[test]
    fn a_promotion_retried_after_an_interrupted_copy_finishes_the_move() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        write_payload(&metadata::client_game_dir(&client.root));
        let destination = registry
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        crate::test_support::interrupt_copy_after(&client.root, &destination, 2);
        assert!(
            !destination.exists(),
            "nothing partial stands at the destination"
        );

        crate::relocate::force_copy(true);
        let promoted = move_client(&registry, client.id, &mut |_| {})
            .expect("the retry replaces what the interrupted copy left");
        crate::relocate::force_copy(false);

        assert_eq!(promoted.root, destination);
        assert_payload(&metadata::client_game_dir(&destination));
    }

    #[test]
    fn a_server_promotion_retried_after_an_interrupted_copy_finishes_the_move() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        assert_eq!(server.storage, StorageClass::Temporary);
        write_payload(&metadata::server_game_dir(&server.root));
        let destination = registry
            .paths()
            .server_root(StorageClass::Persistent, server.id);
        crate::test_support::interrupt_copy_after(&server.root, &destination, 2);

        crate::relocate::force_copy(true);
        let promoted = move_server(&registry, server.id, &mut |_| {})
            .expect("the retry replaces what the interrupted copy left")
            .record;
        crate::relocate::force_copy(false);

        assert_eq!(promoted.root, destination);
        assert_payload(&metadata::server_game_dir(&destination));
        assert!(!server.root.exists());
    }

    #[test]
    fn a_client_edit_stored_while_its_files_copy_survives_the_move() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        write_payload(&metadata::client_game_dir(&client.root));

        let mut edited = false;
        crate::relocate::force_copy(true);
        let promoted = move_client(&registry, client.id, &mut |_| {
            if !edited {
                edited = true;
                registry
                    .modify_client(client.id, |record| {
                        record.name = "renamed meanwhile".to_owned();
                        record.add_attention(AttentionReason::MissingRoot);
                    })
                    .expect("an edit while the files copy");
            }
        })
        .expect("move");
        crate::relocate::force_copy(false);

        assert!(edited, "the edit ran while the copy was in progress");
        assert_eq!(promoted.name, "renamed meanwhile");
        assert_eq!(promoted.storage, StorageClass::Persistent);
        let indexed = registry.client(client.id).expect("indexed");
        let canonical = metadata::read_client_metadata(&promoted.root).expect("metadata");
        for record in [&indexed, &canonical] {
            assert_eq!(record.name, "renamed meanwhile");
            assert_eq!(record.root, promoted.root);
            assert_eq!(record.storage, StorageClass::Persistent);
            assert!(record.attention.contains(&AttentionReason::MissingRoot));
        }
    }

    #[test]
    fn a_server_edit_stored_while_its_files_copy_survives_the_move() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        write_payload(&metadata::server_game_dir(&server.root));

        let mut edited = false;
        crate::relocate::force_copy(true);
        let promoted = move_server(&registry, server.id, &mut |_| {
            if !edited {
                edited = true;
                let mut record = registry.server(server.id).expect("server");
                record.name = "renamed meanwhile".to_owned();
                registry.update_server(&mut record).expect("edit");
            }
        })
        .expect("move")
        .record;
        crate::relocate::force_copy(false);

        assert!(edited, "the edit ran while the copy was in progress");
        assert_eq!(promoted.name, "renamed meanwhile");
        let indexed = registry.server(server.id).expect("indexed");
        let canonical = metadata::read_server_metadata(&promoted.root).expect("metadata");
        for record in [&indexed, &canonical] {
            assert_eq!(record.name, "renamed meanwhile");
            assert_eq!(record.root, promoted.root);
            assert_eq!(record.storage, StorageClass::Persistent);
        }
    }

    #[test]
    fn a_server_move_keeps_canonical_fields_the_index_row_lags_behind() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new(
                "indexed",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        let mut canonical = server.clone();
        canonical.name = "canonical".to_owned();
        canonical.add_attention(AttentionReason::MissingRoot);
        metadata::write_server_metadata(&server.root, &canonical).expect("canonical write");
        assert_eq!(registry.server(server.id).expect("row").name, "indexed");

        let promoted = move_server(&registry, server.id, &mut |_| {})
            .expect("move")
            .record;

        assert_eq!(promoted.name, "canonical");
        let on_disk = metadata::read_server_metadata(&promoted.root).expect("metadata");
        assert_eq!(on_disk.name, "canonical");
        assert!(on_disk.attention.contains(&AttentionReason::MissingRoot));
        assert_eq!(on_disk.root, promoted.root);
        assert_eq!(
            registry.server(server.id).expect("row").name,
            "canonical",
            "the index follows the canonical file"
        );
    }

    #[test]
    fn a_move_refuses_unhealthy_source_metadata_before_copying() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let destination = registry
            .paths()
            .server_root(StorageClass::Persistent, server.id);
        std::fs::write(metadata::server_metadata_path(&server.root), "not json").expect("damage");

        crate::relocate::force_copy(true);
        let error = move_server(&registry, server.id, &mut |_| {})
            .expect_err("unreadable metadata is not moved");
        crate::relocate::force_copy(false);

        assert!(matches!(error, RegistryError::Metadata(_)));
        assert!(!destination.exists(), "nothing was copied");
        assert!(server.root.is_dir());
    }

    #[test]
    fn a_move_refuses_while_another_move_of_the_same_instance_runs() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let _running = registry
            .claim_host(RecordRef::Client(client.id))
            .expect("claim");

        let error = move_client(&registry, client.id, &mut |_| {})
            .expect_err("the first move still holds the claim");

        assert!(matches!(error, RegistryError::HostBusy { .. }));
        assert!(client.root.is_dir());
    }

    #[test]
    fn a_recipe_claim_held_by_another_process_is_seen_and_released_with_it() {
        use rustix::fs::{FlockOperation, flock};

        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let host = RecordRef::Client(InstanceId::new());
        let guards = registry.paths().move_guards_dir();
        std::fs::create_dir_all(&guards).expect("guard directory");
        // Through its own descriptor with no in-process claim, so only the
        // kernel lock shows it, as another process would.
        let other_process = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(guards.join(recipe_guard_name(host)))
            .expect("open the guard");
        flock(&other_process, FlockOperation::NonBlockingLockExclusive).expect("lock");

        assert!(registry.try_claim_recipe(host).expect("try").is_none());

        drop(other_process);
        let claim = registry.try_claim_recipe(host).expect("try");
        assert!(claim.is_some());
        assert!(
            registry.try_claim_recipe(host).expect("try").is_none(),
            "one claim at a time within this process too"
        );
    }

    fn guard_files(registry: &Registry) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(registry.paths().move_guards_dir())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Claims the host and its recipe, so both guard files exist.
    fn touch_both_guards(registry: &Registry, host: RecordRef) {
        drop(registry.claim_host(host).expect("claim"));
        drop(registry.try_claim_recipe(host).expect("try"));
        assert_eq!(guard_files(registry).len(), 2, "both guard files exist");
    }

    #[test]
    fn deleting_a_client_removes_its_host_and_recipe_guard_files() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        touch_both_guards(&registry, RecordRef::Client(client.id));

        registry
            .delete_client(client.id, crate::FileDisposition::Remove)
            .expect("delete");

        assert_eq!(guard_files(&registry), Vec::<String>::new());
    }

    #[test]
    fn deleting_a_server_removes_its_host_and_recipe_guard_files() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        touch_both_guards(&registry, RecordRef::Server(server.id));

        registry
            .delete_server(server.id, crate::FileDisposition::Remove)
            .expect("delete");

        assert_eq!(guard_files(&registry), Vec::<String>::new());
    }

    #[test]
    fn deleting_a_host_under_its_own_claim_removes_the_guard_as_the_claim_is_released() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let host = RecordRef::Client(client.id);
        let claim = registry.claim_host(host).expect("claim");
        let recipe = registry.try_claim_recipe(host).expect("try").expect("free");

        registry
            .delete_client(client.id, crate::FileDisposition::Remove)
            .expect("delete");
        assert_eq!(
            guard_files(&registry).len(),
            2,
            "a held claim keeps its file until it is released"
        );
        drop(claim);
        drop(recipe);

        assert_eq!(guard_files(&registry), Vec::<String>::new());
    }

    #[test]
    fn deleting_a_host_leaves_the_claim_files_of_other_hosts() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let gone = registry
            .create_published_client(NewClient::new("gone", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let kept = registry
            .create_published_client(NewClient::new("kept", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        touch_both_guards(&registry, RecordRef::Client(gone.id));
        drop(
            registry
                .claim_host(RecordRef::Client(kept.id))
                .expect("claim"),
        );

        registry
            .delete_client(gone.id, crate::FileDisposition::Remove)
            .expect("delete");

        assert_eq!(
            guard_files(&registry),
            vec![host_guard_name(RecordRef::Client(kept.id))]
        );
    }

    #[test]
    fn a_pruning_sweep_removes_only_unknown_hosts_claim_files_that_nothing_holds() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let indexed = registry
            .create_published_client(NewClient::new("kept", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let indexed_host = RecordRef::Client(indexed.id);
        drop(registry.claim_host(indexed_host).expect("claim"));
        let unknown_client = RecordRef::Client(InstanceId::new());
        let unknown_server = RecordRef::Server(ServerId::new());
        let held = RecordRef::Client(InstanceId::new());
        for host in [unknown_client, unknown_server] {
            touch_both_guards_unchecked(&registry, host);
        }
        let _held = claim_host(&registry.paths, held, Path::new("unindexed")).expect("claim");
        let stranger = registry.paths().move_guards_dir().join("notes.guard");
        std::fs::write(&stranger, b"not ours").expect("write");

        registry.prune_unknown_host_guards();

        let mut expected = vec![
            host_guard_name(indexed_host),
            host_guard_name(held),
            "notes.guard".to_owned(),
        ];
        expected.sort();
        assert_eq!(guard_files(&registry), expected);
        assert!(stranger.is_file());
    }

    fn touch_both_guards_unchecked(registry: &Registry, host: RecordRef) {
        drop(claim_host(&registry.paths, host, Path::new("unindexed")).expect("claim"));
        drop(registry.try_claim_recipe(host).expect("try"));
    }

    #[test]
    fn a_claim_file_held_by_another_process_survives_the_sweep_until_it_is_released() {
        use rustix::fs::{FlockOperation, flock};

        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let host = RecordRef::Client(InstanceId::new());
        let guards = registry.paths().move_guards_dir();
        std::fs::create_dir_all(&guards).expect("guard directory");
        let other_process = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(guards.join(host_guard_name(host)))
            .expect("open the guard");
        flock(&other_process, FlockOperation::NonBlockingLockExclusive).expect("lock");

        registry.prune_unknown_host_guards();

        assert_eq!(guard_files(&registry), vec![host_guard_name(host)]);
        drop(other_process);
        registry.prune_unknown_host_guards();
        assert_eq!(guard_files(&registry), Vec::<String>::new());
    }

    #[test]
    fn a_recipe_claim_does_not_block_a_restore_or_move_claim_of_the_same_host() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let _recipe = registry
            .try_claim_recipe(RecordRef::Client(client.id))
            .expect("try")
            .expect("free");

        registry
            .claim_host(RecordRef::Client(client.id))
            .expect("the tree claim is independent");
    }

    #[cfg(unix)]
    #[test]
    fn a_move_whose_metadata_directory_cannot_sync_is_recorded_and_warns() {
        use std::os::unix::fs::PermissionsExt as _;

        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let destination = registry
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        std::fs::create_dir_all(destination.parent().expect("parent")).expect("mkdir");
        std::fs::rename(&client.root, &destination).expect("the move's rename");
        let directory = metadata::client_metadata_path(&destination)
            .parent()
            .expect("metadata directory")
            .to_path_buf();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o300))
            .expect("restrict metadata directory");
        let unsyncable = std::fs::File::open(&directory).is_err();
        let result = unsyncable.then(|| {
            let claim = registry
                .claim_host(RecordRef::Client(client.id))
                .expect("claim");
            registry.relocate_client(client.id, &claim, StorageClass::Persistent, &mut |_| {})
        });
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .expect("restore metadata directory");
        let Some(result) = result else {
            return;
        };

        let committed = result.expect("the published metadata is a committed move");
        assert_eq!(committed.record.storage, StorageClass::Persistent);
        assert!(matches!(
            committed.warnings.as_slice(),
            [ClientCommitWarning::MetadataNotDurable { .. }]
        ));
        assert_eq!(
            registry.client(client.id).expect("row").storage,
            StorageClass::Persistent
        );
    }

    #[test]
    fn a_retry_finishes_a_rename_an_earlier_move_could_not_record() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let destination = registry
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        std::fs::create_dir_all(destination.parent().expect("parent")).expect("mkdir");
        std::fs::rename(&client.root, &destination).expect("the earlier move's rename");

        let promoted =
            move_client(&registry, client.id, &mut |_| {}).expect("the retry records the rename");

        assert_eq!(promoted.root, destination);
        assert_eq!(
            registry.client(client.id).expect("indexed").root,
            destination
        );
    }

    /// Moves a client whose index writes fail, returning it with the root it
    /// left and the root it moved to.
    fn move_client_with_failing_index(
        registry: &Registry,
        client: &ClientRecord,
    ) -> (ClientCommit<ClientRecord>, PathBuf) {
        let destination = registry
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        fail_index_writes(registry, "instances");
        let claim = registry
            .claim_host(RecordRef::Client(client.id))
            .expect("claim");
        let committed = registry
            .relocate_client(client.id, &claim, StorageClass::Persistent, &mut |_| {})
            .expect("the published metadata is a committed move");
        (committed, destination)
    }

    #[test]
    fn a_move_whose_index_write_fails_is_recorded_and_warns() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = created_client(&registry);

        let (committed, destination) = move_client_with_failing_index(&registry, &client);

        assert!(matches!(
            committed.warnings.as_slice(),
            [ClientCommitWarning::IndexOutOfSync { .. }]
        ));
        assert_eq!(committed.record.root, destination);
        assert_eq!(committed.record.storage, StorageClass::Persistent);
        let canonical = metadata::read_client_metadata(&destination).expect("metadata");
        assert_eq!(canonical.storage, StorageClass::Persistent);
        assert_eq!(canonical.root, destination);
        assert_eq!(
            registry.indexed_client(client.id).expect("row").root,
            client.root
        );
    }

    #[test]
    fn a_client_stays_addressable_at_its_new_root_when_the_move_could_not_update_the_index() {
        for copy in [false, true] {
            let environment = TestEnvironment::new();
            let registry = environment.registry();
            let client = created_client(&registry);

            crate::relocate::force_copy(copy);
            let (_, destination) = move_client_with_failing_index(&registry, &client);
            crate::relocate::force_copy(false);
            restore_index_writes(&registry);
            assert!(
                !client.root.exists() || !copy,
                "a copy's source is discarded"
            );

            let renamed = registry
                .modify_client(client.id, |record| record.name = "after".to_owned())
                .expect("the change follows the moved files");

            assert_eq!(renamed.root, destination);
            assert_eq!(renamed.storage, StorageClass::Persistent);
            assert_eq!(
                metadata::read_client_metadata(&destination)
                    .expect("metadata")
                    .name,
                "after",
                "copy: {copy}"
            );
            let row = registry.client(client.id).expect("row");
            assert_eq!(row.root, destination, "copy: {copy}");
            assert_eq!(row.storage, StorageClass::Persistent);
            assert_eq!(row.name, "after");
        }
    }

    #[test]
    fn a_change_to_a_vanished_temporary_client_is_not_redirected_when_nothing_moved() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = created_client(&registry);
        std::fs::remove_dir_all(&client.root).expect("the OS clears the temporary root");

        let record = registry
            .modify_client(client.id, |record| record.name = "after".to_owned())
            .expect("only the row changes");

        assert_eq!(record.root, client.root);
        assert_eq!(record.storage, StorageClass::Temporary);
    }

    #[test]
    fn a_client_whose_move_could_not_update_the_index_is_read_at_its_persistent_root() {
        for copy in [false, true] {
            let environment = TestEnvironment::new();
            let registry = environment.registry();
            let client = created_client(&registry);

            crate::relocate::force_copy(copy);
            let (_, destination) = move_client_with_failing_index(&registry, &client);
            crate::relocate::force_copy(false);

            let read = registry.client(client.id).expect("read");
            assert_eq!(read.root, destination, "copy: {copy}");
            assert_eq!(read.storage, StorageClass::Persistent, "copy: {copy}");
            assert_eq!(
                registry.indexed_client(client.id).expect("row").root,
                client.root,
                "the row itself is not rewritten by a read"
            );

            let duplicated = registry
                .duplicate_client(client.id, &DuplicateClient::default())
                .expect("a duplicate copies the files where they are");
            assert!(duplicated.record.root.is_dir(), "copy: {copy}");
        }
    }

    #[test]
    fn a_client_that_vanished_without_a_move_is_still_read_at_the_root_the_index_names() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = created_client(&registry);
        std::fs::remove_dir_all(&client.root).expect("the OS clears the temporary root");

        let read = registry.client(client.id).expect("read");

        assert_eq!(read.root, client.root);
        assert_eq!(read.storage, StorageClass::Temporary);
    }

    #[test]
    fn deleting_a_client_whose_move_could_not_update_the_index_removes_the_files_where_they_are() {
        for copy in [false, true] {
            let environment = TestEnvironment::new();
            let registry = environment.registry();
            let client = created_client(&registry);

            crate::relocate::force_copy(copy);
            let (_, destination) = move_client_with_failing_index(&registry, &client);
            crate::relocate::force_copy(false);
            restore_index_writes(&registry);

            registry
                .delete_client(client.id, FileDisposition::Remove)
                .expect("delete");

            assert!(!destination.exists(), "copy: {copy}");
            assert!(!client.root.exists(), "copy: {copy}");
            let report = crate::reconcile::reconcile(&registry).expect("reconcile");
            assert!(report.reindexed.is_empty(), "copy: {copy}");
            assert!(registry.list_clients().expect("list").is_empty());
        }
    }

    #[test]
    fn retrying_a_move_whose_index_write_failed_repairs_the_index_row() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = created_client(&registry);
        let (_, destination) = move_client_with_failing_index(&registry, &client);
        restore_index_writes(&registry);

        let claim = registry
            .claim_host(RecordRef::Client(client.id))
            .expect("claim");
        let retried = registry
            .relocate_client(client.id, &claim, StorageClass::Persistent, &mut |_| {})
            .expect("retry");

        assert!(retried.warnings.is_empty());
        let row = registry.indexed_client(client.id).expect("row");
        assert_eq!(row.root, destination);
        assert_eq!(row.storage, StorageClass::Persistent);
    }

    fn prepare_unfinished_restore(root: &Path, part: crate::restore::RestorePart) {
        let transaction = crate::restore::prepare_restore(
            root,
            ephervex_model::SnapshotId::new(),
            vec![crate::restore::RestorePartState {
                part,
                original_present: true,
                replacement_present: true,
            }],
        )
        .expect("prepare");
        let incoming = transaction.incoming_path(part);
        std::fs::create_dir_all(incoming.parent().expect("parent")).expect("incoming");
        std::fs::copy(root.join(".launcher/instance.json"), &incoming).expect("replacement");
        transaction.publish().expect("publish, never finished");
    }

    /// Recovery puts the pre-restore metadata back, which would contradict a
    /// move recorded in between.
    #[test]
    fn a_move_refuses_a_host_whose_snapshot_restore_is_unfinished() {
        for copy in [false, true] {
            let environment = TestEnvironment::new();
            let registry = environment.registry();
            let client = registry
                .create_published_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
                .expect("create");
            prepare_unfinished_restore(&client.root, crate::restore::RestorePart::ClientMetadata);

            crate::relocate::force_copy(copy);
            let error = move_client(&registry, client.id, &mut |_| {})
                .expect_err("startup recovers the restore first");
            crate::relocate::force_copy(false);

            assert!(
                matches!(error, RegistryError::RestoreUnfinished { .. }),
                "copy: {copy}"
            );
            assert!(client.root.is_dir());
            assert_eq!(
                registry.client(client.id).expect("indexed").storage,
                StorageClass::Temporary
            );
            let destination = registry
                .paths()
                .instance_root(StorageClass::Persistent, client.id);
            assert!(!destination.exists());
        }
    }

    #[test]
    fn a_move_never_replaces_the_tree_the_record_already_names() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        write_payload(&metadata::client_game_dir(&client.root));
        let persistent = registry
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        std::fs::create_dir_all(persistent.parent().expect("parent")).expect("mkdir");
        std::fs::rename(&client.root, &persistent).expect("the host stands at the persistent path");
        let mut stale = client.clone();
        stale.root.clone_from(&persistent);
        registry
            .update_client(&mut stale)
            .expect("a record with the wrong class");

        let promoted =
            move_client(&registry, client.id, &mut |_| {}).expect("only the record was stale");

        assert_eq!(promoted.storage, StorageClass::Persistent);
        assert_eq!(promoted.root, persistent);
        assert_payload(&metadata::client_game_dir(&persistent));
    }

    #[test]
    fn a_move_refuses_a_destination_holding_another_instance_s_files() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let other = registry
            .create_published_client(NewClient::new("y", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let destination = registry
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        crate::relocate::force_copy(true);
        crate::relocate::relocate_tree(&other.root, &destination, &mut |_| {}).expect("copy");
        crate::relocate::force_copy(false);

        let error =
            move_client(&registry, client.id, &mut |_| {}).expect_err("somebody else's files");

        assert!(matches!(error, RegistryError::RootOccupied { .. }));
        assert!(destination.join(".launcher/instance.json").is_file());
        assert!(client.root.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn a_metadata_directory_sync_failure_still_updates_the_index_and_warns() {
        use std::os::unix::fs::PermissionsExt as _;

        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new(
                "before",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        let directory = metadata::server_metadata_path(&server.root)
            .parent()
            .expect("metadata directory")
            .to_path_buf();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o300))
            .expect("restrict metadata directory");
        let unsyncable = std::fs::File::open(&directory).is_err();
        let mut record = server.clone();
        record.name = "after".to_owned();
        let result = unsyncable.then(|| registry.update_server_committed(&mut record));
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .expect("restore metadata directory");
        let Some(result) = result else {
            return;
        };

        let commit = result.expect("the published metadata is a committed mutation");
        assert!(matches!(
            commit.warnings.as_slice(),
            [ServerCommitWarning::MetadataNotDurable { .. }]
        ));
        assert_eq!(
            metadata::read_server_metadata(&server.root)
                .expect("metadata")
                .name,
            "after"
        );
        assert_eq!(registry.server(server.id).expect("row").name, "after");
    }

    #[cfg(unix)]
    #[test]
    fn a_client_metadata_directory_sync_failure_still_updates_the_index_and_warns() {
        use std::os::unix::fs::PermissionsExt as _;

        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new(
                "before",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        let directory = metadata::client_metadata_path(&client.root)
            .parent()
            .expect("metadata directory")
            .to_path_buf();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o300))
            .expect("restrict metadata directory");
        let unsyncable = std::fs::File::open(&directory).is_err();
        let mut record = client.clone();
        record.name = "after".to_owned();
        let result = unsyncable.then(|| registry.update_client_committed(&mut record));
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .expect("restore metadata directory");
        let Some(result) = result else {
            return;
        };

        let warnings = result.expect("the published metadata is a committed mutation");
        assert!(matches!(
            warnings.as_slice(),
            [ClientCommitWarning::MetadataNotDurable { .. }]
        ));
        assert_eq!(
            metadata::read_client_metadata(&client.root)
                .expect("metadata")
                .name,
            "after"
        );
        assert_eq!(registry.client(client.id).expect("row").name, "after");
    }

    fn created_client(registry: &Registry) -> ClientRecord {
        registry
            .create_published_client(NewClient::new(
                "before",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create")
    }

    #[test]
    fn a_client_index_write_failure_after_publication_is_a_warning_on_a_committed_update() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = created_client(&registry);
        registry
            .index()
            .connection()
            .execute_batch(
                "CREATE TRIGGER refuse_instance_updates BEFORE UPDATE ON instances
                 BEGIN SELECT RAISE(ABORT, 'index unavailable'); END;",
            )
            .expect("install failure");
        let mut record = client.clone();
        record.name = "after".to_owned();

        let warnings = registry
            .update_client_committed(&mut record)
            .expect("the published metadata is a committed mutation");

        assert!(matches!(
            warnings.as_slice(),
            [ClientCommitWarning::IndexOutOfSync { .. }]
        ));
        assert_eq!(
            metadata::read_client_metadata(&client.root)
                .expect("metadata")
                .name,
            "after"
        );
        assert_eq!(registry.client(client.id).expect("row").name, "before");
        let mut again = record.clone();
        again.name = "again".to_owned();
        registry
            .update_client(&mut again)
            .expect("callers of the plain update see a committed update");
    }

    #[test]
    fn a_client_missing_from_the_index_is_refused_before_its_metadata_is_written() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = created_client(&registry);
        registry
            .index()
            .connection()
            .execute(
                "DELETE FROM instances WHERE id = ?1",
                params![client.id.to_string()],
            )
            .expect("drop the row");
        let mut record = client.clone();
        record.name = "after".to_owned();

        let error = registry
            .update_client_committed(&mut record)
            .expect_err("an unlisted instance is not mutable");

        assert!(matches!(error, RegistryError::InstanceUnknown { .. }));
        assert_eq!(
            metadata::read_client_metadata(&client.root)
                .expect("metadata")
                .name,
            "before"
        );
    }

    #[test]
    fn a_client_update_fails_when_the_index_fails_and_no_metadata_was_published() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = created_client(&registry);
        std::fs::remove_dir_all(&client.root).expect("remove the instance root");
        registry
            .index()
            .connection()
            .execute_batch(
                "CREATE TRIGGER refuse_instance_updates BEFORE UPDATE ON instances
                 BEGIN SELECT RAISE(ABORT, 'index unavailable'); END;",
            )
            .expect("install failure");
        let mut record = client.clone();
        record.name = "after".to_owned();

        let error = registry
            .update_client_committed(&mut record)
            .expect_err("the index is the only record, so its failure is the update's failure");

        assert!(matches!(error, RegistryError::Index(_)));
    }

    fn fail_index_writes(registry: &Registry, table: &str) {
        registry
            .index()
            .connection()
            .execute_batch(&format!(
                "CREATE TRIGGER refuse_index_writes BEFORE UPDATE ON {table}
                 BEGIN SELECT RAISE(ABORT, 'index is read-only'); END;"
            ))
            .expect("install failing trigger");
    }

    fn restore_index_writes(registry: &Registry) {
        registry
            .index()
            .connection()
            .execute_batch("DROP TRIGGER refuse_index_writes;")
            .expect("drop failing trigger");
    }

    #[test]
    fn a_client_change_after_a_lagging_index_write_keeps_the_committed_fields() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = created_client(&registry);

        fail_index_writes(&registry, "instances");
        registry
            .modify_client(client.id, |record| record.name = "renamed".to_owned())
            .expect("the published metadata is a committed mutation");
        restore_index_writes(&registry);
        assert_eq!(registry.client(client.id).expect("row").name, "before");

        let launched = registry
            .modify_client(client.id, |record| {
                record.last_launched_at = Some(Utc::now());
            })
            .expect("later change");

        assert_eq!(launched.name, "renamed");
        let canonical = metadata::read_client_metadata(&client.root).expect("metadata");
        let indexed = registry.client(client.id).expect("row");
        for record in [&canonical, &indexed] {
            assert_eq!(record.name, "renamed");
            assert!(record.last_launched_at.is_some());
        }
    }

    #[test]
    fn a_client_change_with_a_missing_root_starts_from_the_index_row() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("kept", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        std::fs::remove_dir_all(&client.root).expect("remove root");

        let changed = registry
            .modify_client(client.id, |record| {
                record.add_attention(AttentionReason::MissingRoot);
            })
            .expect("index-only change");

        assert_eq!(changed.name, "kept");
        assert!(changed.attention.contains(&AttentionReason::MissingRoot));
    }

    #[test]
    fn a_client_change_refuses_metadata_that_names_another_instance() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("kept", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let mut foreign = client.clone();
        foreign.id = InstanceId::new();
        metadata::write_client_metadata(&client.root, &foreign).expect("foreign write");

        let error = registry
            .modify_client(client.id, |record| record.name = "changed".to_owned())
            .expect_err("foreign metadata blocks the change");

        assert!(
            matches!(&error, RegistryError::InstanceMetadataForeign { path } if *path == client.root),
            "{error}"
        );
        assert_eq!(
            error.message_id(),
            "error-registry-instance-metadata-foreign"
        );
        assert_eq!(
            metadata::read_client_metadata(&client.root)
                .expect("metadata")
                .id,
            foreign.id
        );
    }

    #[test]
    fn moving_a_client_refuses_metadata_that_names_another_instance_and_leaves_it_in_place() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("kept", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let mut foreign = client.clone();
        foreign.id = InstanceId::new();
        metadata::write_client_metadata(&client.root, &foreign).expect("foreign write");
        let claim = registry
            .claim_host(RecordRef::Client(client.id))
            .expect("claim");

        let error = registry
            .relocate_client(client.id, &claim, StorageClass::Persistent, &mut |_| {})
            .expect_err("foreign metadata refuses the move");

        assert!(
            matches!(&error, RegistryError::InstanceMetadataForeign { path } if *path == client.root),
            "{error}"
        );
        assert!(client.root.is_dir(), "the files stay where they were");
        let row = registry.client(client.id).expect("row");
        assert_eq!(row.root, client.root);
        assert_eq!(row.storage, client.storage);
    }

    #[test]
    fn moving_a_server_refuses_metadata_that_names_another_server_and_leaves_it_in_place() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new("kept", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let mut foreign = server.clone();
        foreign.id = ServerId::new();
        metadata::write_server_metadata(&server.root, &foreign).expect("foreign write");
        let claim = registry
            .claim_host(RecordRef::Server(server.id))
            .expect("claim");

        let error = registry
            .relocate_server(server.id, &claim, StorageClass::Persistent, &mut |_| {})
            .expect_err("foreign metadata refuses the move");

        assert!(
            matches!(&error, RegistryError::ServerMetadataForeign { path } if *path == server.root),
            "{error}"
        );
        assert_eq!(error.message_id(), "error-registry-server-metadata-foreign");
        assert!(server.root.is_dir(), "the files stay where they were");
        let row = registry.server(server.id).expect("row");
        assert_eq!(row.root, server.root);
        assert_eq!(row.storage, server.storage);
    }

    #[test]
    fn a_client_whose_root_holds_another_instances_metadata_is_forgotten_but_its_files_are_not_removed()
     {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("kept", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let mut foreign = client.clone();
        foreign.id = InstanceId::new();
        metadata::write_client_metadata(&client.root, &foreign).expect("foreign write");

        let error = registry
            .delete_client(client.id, crate::FileDisposition::Remove)
            .expect_err("the directory is not verifiably this instance's");

        assert!(
            matches!(&error, RegistryError::InstanceMetadataForeign { path } if *path == client.root),
            "{error}"
        );
        assert!(client.root.is_dir());
        assert!(registry.client(client.id).is_ok(), "the record is kept");

        registry
            .delete_client(client.id, crate::FileDisposition::Keep)
            .expect("the row is forgotten");

        assert!(client.root.is_dir(), "the files are kept");
        assert!(matches!(
            registry.client(client.id),
            Err(RegistryError::InstanceUnknown { .. })
        ));
    }

    #[test]
    fn a_server_whose_root_holds_another_servers_metadata_is_forgotten_without_reading_it() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new("kept", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let mut foreign = server.clone();
        foreign.id = ServerId::new();
        metadata::write_server_metadata(&server.root, &foreign).expect("foreign write");
        let before = std::fs::read(metadata::server_metadata_path(&server.root)).expect("read");

        registry
            .delete_server(server.id, crate::FileDisposition::Keep)
            .expect("the row is forgotten");

        assert!(matches!(
            registry.server(server.id),
            Err(RegistryError::ServerUnknown { .. })
        ));
        assert_eq!(
            std::fs::read(metadata::server_metadata_path(&server.root)).expect("read"),
            before,
            "the directory is left untouched"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_modified_client_whose_metadata_directory_cannot_sync_returns_the_warning_with_the_record()
    {
        use std::os::unix::fs::PermissionsExt as _;

        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = created_client(&registry);
        let directory = metadata::client_metadata_path(&client.root)
            .parent()
            .expect("metadata directory")
            .to_path_buf();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o300))
            .expect("restrict metadata directory");
        let unsyncable = std::fs::File::open(&directory).is_err();
        let result = unsyncable.then(|| {
            registry.modify_client_committed(client.id, |current| {
                current.name = "after".to_owned();
            })
        });
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .expect("restore metadata directory");
        let Some(result) = result else {
            return;
        };

        let committed = result.expect("the published metadata is a committed change");
        assert_eq!(committed.record.name, "after");
        assert!(matches!(
            committed.warnings.as_slice(),
            [ClientCommitWarning::MetadataNotDurable { .. }]
        ));
        assert_eq!(registry.client(client.id).expect("row").name, "after");
    }

    /// Commits a loader and Java override change whose index write fails, so
    /// the row keeps the loader and overrides the client was created with.
    fn client_with_lagging_loader_and_overrides(
        registry: &Registry,
    ) -> (ClientRecord, LoaderSelection) {
        let client = registry
            .create_published_client(NewClient::new(
                "client",
                "1.21.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "21.1.100"),
            ))
            .expect("create");
        let loader = LoaderSelection::exact(LoaderKind::NeoForge, "21.1.172");

        fail_index_writes(registry, "instances");
        registry
            .modify_client(client.id, |record| {
                record.loader = loader.clone();
                record.java_override = Some(JavaOverride::new("/opt/jdk/bin/java"));
            })
            .expect("the published metadata is a committed mutation");
        restore_index_writes(registry);
        let lagging = registry.client(client.id).expect("row");
        assert_eq!(lagging.loader.version.as_deref(), Some("21.1.100"));
        assert!(lagging.java_override.is_none());
        (client, loader)
    }

    #[test]
    fn the_current_client_is_the_canonical_record_while_the_index_row_lags() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let (client, loader) = client_with_lagging_loader_and_overrides(&registry);

        let current = registry.current_client(client.id).expect("current");

        assert_eq!(current.loader, loader);
        assert_eq!(
            current.java_override,
            Some(JavaOverride::new("/opt/jdk/bin/java"))
        );
        assert_eq!(current.root, client.root);
    }

    #[test]
    fn a_duplicate_takes_the_committed_settings_the_index_row_lags_behind() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let (client, loader) = client_with_lagging_loader_and_overrides(&registry);

        let duplicate = registry
            .duplicate_client(client.id, &DuplicateClient::default())
            .expect("duplicate")
            .record;

        assert_eq!(duplicate.loader, loader);
        assert_eq!(
            duplicate.java_override,
            Some(JavaOverride::new("/opt/jdk/bin/java"))
        );
        let canonical = metadata::read_client_metadata(&duplicate.root).expect("metadata");
        assert_eq!(canonical.loader, loader);
        assert_eq!(registry.client(duplicate.id).expect("row").loader, loader);
    }

    #[test]
    fn a_linked_server_takes_the_loader_the_index_row_lags_behind() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let (client, loader) = client_with_lagging_loader_and_overrides(&registry);

        let server = registry
            .prepare_server(NewServer::linked("server", client.id))
            .expect("prepare linked server");

        assert_eq!(server.loader, loader);
        assert_eq!(server.minecraft_version, "1.21.1");
    }

    #[test]
    fn a_linked_server_is_refused_when_the_clients_metadata_names_another_instance() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = created_client(&registry);
        let mut foreign = client.clone();
        foreign.id = InstanceId::new();
        metadata::write_client_metadata(&client.root, &foreign).expect("foreign write");

        let error = registry
            .prepare_server(NewServer::linked("server", client.id))
            .expect_err("foreign metadata blocks the selection");

        assert!(
            matches!(&error, RegistryError::InstanceMetadataForeign { path } if *path == client.root),
            "{error:?}"
        );
    }

    #[test]
    fn a_client_with_a_missing_root_is_current_from_the_index_row() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = created_client(&registry);
        std::fs::remove_dir_all(&client.root).expect("remove root");

        let current = registry.current_client(client.id).expect("current");

        assert_eq!(current.name, "before");
    }

    #[test]
    fn deleting_a_client_refuses_a_linked_server_root_holding_another_servers_metadata() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("c", "1.20.1", LoaderSelection::vanilla()))
            .expect("client");
        let linked = registry
            .create_server(NewServer::linked("linked", client.id))
            .expect("linked");
        let other = registry
            .create_server(NewServer::new(
                "other",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("other");
        let mut foreign = other.clone();
        foreign.linked_client_id = Some(client.id);
        metadata::write_server_metadata(&linked.root, &foreign).expect("copied directory");
        let other_before =
            std::fs::read(metadata::server_metadata_path(&other.root)).expect("read");
        let other_row_before = registry.server(other.id).expect("row");

        let error = registry
            .delete_client(client.id, crate::FileDisposition::Remove)
            .expect_err("foreign metadata refuses the deletion");

        assert!(
            matches!(&error, RegistryError::ServerMetadataForeign { path } if *path == linked.root),
            "{error:?}"
        );
        assert_eq!(
            std::fs::read(metadata::server_metadata_path(&other.root)).expect("read"),
            other_before,
            "the other server's metadata is untouched"
        );
        let other_row_after = registry.server(other.id).expect("row");
        assert_eq!(other_row_after.updated_at, other_row_before.updated_at);
        assert_eq!(other_row_after.linked_client_id, None);
        assert!(client.root.is_dir(), "no files were removed");
        assert!(registry.client(client.id).is_ok(), "the client is kept");
    }

    #[test]
    fn deleting_a_client_checks_every_linked_server_before_changing_any() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("c", "1.20.1", LoaderSelection::vanilla()))
            .expect("client");
        let healthy = registry
            .create_server(NewServer::linked("a-healthy", client.id))
            .expect("healthy");
        let damaged = registry
            .create_server(NewServer::linked("b-damaged", client.id))
            .expect("damaged");
        std::fs::write(metadata::server_metadata_path(&damaged.root), "not json").expect("damage");

        let error = registry
            .delete_client(client.id, crate::FileDisposition::Keep)
            .expect_err("unreadable metadata refuses the deletion");

        assert!(matches!(error, RegistryError::Metadata(_)));
        let on_disk = metadata::read_server_metadata(&healthy.root).expect("metadata");
        assert_eq!(on_disk.linked_client_id, Some(client.id));
        assert_eq!(
            registry.server(healthy.id).expect("row").linked_client_id,
            Some(client.id)
        );
    }

    #[test]
    fn deleting_a_client_unlinks_a_server_at_its_indexed_root_even_when_its_metadata_names_a_stale_root()
     {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("c", "1.20.1", LoaderSelection::vanilla()))
            .expect("client");
        let server = registry
            .create_server(NewServer::linked("s", client.id))
            .expect("server");
        let mut stale = server.clone();
        stale.root = environment.base().join("elsewhere");
        metadata::write_server_metadata(&server.root, &stale).expect("stale root");

        registry
            .delete_client(client.id, crate::FileDisposition::Keep)
            .expect("delete");

        let on_disk = metadata::read_server_metadata(&server.root).expect("metadata");
        assert_eq!(on_disk.linked_client_id, None);
        assert_eq!(on_disk.root, server.root, "published at the indexed root");
        assert!(!environment.base().join("elsewhere").exists());
        assert_eq!(registry.server(server.id).expect("row").root, server.root);
    }

    #[test]
    fn deleting_a_client_keeps_the_indexed_storage_class_of_a_linked_server() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("c", "1.20.1", LoaderSelection::vanilla()))
            .expect("client");
        let server = registry
            .create_server(NewServer::linked("s", client.id))
            .expect("server");
        let mut contradicting = server.clone();
        contradicting.storage = match server.storage {
            StorageClass::Temporary => StorageClass::Persistent,
            StorageClass::Persistent => StorageClass::Temporary,
        };
        metadata::write_server_metadata(&server.root, &contradicting).expect("storage class");

        registry
            .delete_client(client.id, crate::FileDisposition::Keep)
            .expect("delete");

        let on_disk = metadata::read_server_metadata(&server.root).expect("metadata");
        assert_eq!(on_disk.storage, server.storage);
        assert_eq!(
            registry.server(server.id).expect("row").storage,
            server.storage
        );
    }

    /// A client changed while its root was missing, whose older metadata file
    /// is back in place before reconciliation has run.
    fn client_changed_while_its_root_was_missing(registry: &Registry) -> ClientRecord {
        let client = created_client(registry);
        let older = metadata::read_client_metadata(&client.root).expect("metadata");
        std::fs::remove_dir_all(&client.root).expect("remove root");
        let changed = registry
            .modify_client(client.id, |record| {
                record.add_attention(AttentionReason::MissingRoot);
                record.java_override = Some(JavaOverride::new("/opt/jdk/bin/java"));
                record.name = "renamed".to_owned();
            })
            .expect("index-only change");
        std::fs::create_dir_all(&client.root).expect("restore root");
        metadata::write_client_metadata(&client.root, &older).expect("restore metadata");
        changed
    }

    #[test]
    fn a_newer_index_row_is_current_once_a_missing_root_returns() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let changed = client_changed_while_its_root_was_missing(&registry);

        let current = registry.current_client(changed.id).expect("current");

        assert_eq!(current.name, "renamed");
        assert_eq!(
            current.java_override,
            Some(JavaOverride::new("/opt/jdk/bin/java"))
        );
        assert!(!current.attention.contains(&AttentionReason::MissingRoot));
    }

    #[test]
    fn a_client_change_after_a_missing_root_returns_keeps_the_index_only_edits() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let changed = client_changed_while_its_root_was_missing(&registry);

        let updated = registry
            .modify_client(changed.id, |record| {
                record.last_launched_at = Some(Utc::now());
            })
            .expect("change");

        assert_eq!(updated.name, "renamed");
        assert_eq!(
            updated.java_override,
            Some(JavaOverride::new("/opt/jdk/bin/java"))
        );
        let canonical = metadata::read_client_metadata(&updated.root).expect("metadata");
        assert_eq!(canonical.name, "renamed");
        assert_eq!(canonical.java_override, updated.java_override);
        assert!(!canonical.attention.contains(&AttentionReason::MissingRoot));
    }

    #[test]
    fn an_index_row_without_the_missing_root_flag_never_outranks_the_metadata() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = created_client(&registry);
        let mut newer_row = registry.client(client.id).expect("row");
        newer_row.name = "index only".to_owned();
        newer_row.updated_at = client.updated_at + chrono::Duration::seconds(60);
        registry
            .synchronize_client_index(&newer_row)
            .expect("write the row");
        assert!(
            newer_row.updated_at
                > metadata::read_client_metadata(&client.root)
                    .expect("metadata")
                    .updated_at
        );
        assert!(!newer_row.attention.contains(&AttentionReason::MissingRoot));

        let current = registry.current_client(client.id).expect("current");

        assert_eq!(current.name, "before");
    }
}
