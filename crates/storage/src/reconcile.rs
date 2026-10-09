//! Startup reconciliation between the index and the filesystem
//! (ADR 0006).
//!
//! Three rules, and the whole module exists to keep them apart:
//!
//! 1. **A vanished temporary root is expected.** It means the operating system
//!    cleared its temporary directory, which is precisely the lifecycle the
//!    product delegates to it. The index entry is dropped silently - no
//!    warning, no "needs attention", nothing for the user to dismiss.
//! 2. **A vanished persistent root is not.** The record is kept and marked
//!    [`AttentionReason::MissingRoot`] so an unmounted disk or a moved
//!    directory is recoverable. Nothing is ever deleted on the user's behalf.
//! 3. **A directory the index has never heard of is an opportunity.** A
//!    `.launcher/instance.json` under the persistent instance root is re-indexed,
//!    which is what makes losing `launcher.sqlite` a rebuild rather than a loss
//!    (ADR 0002).
//!
//! The temporary/persistent distinction is not re-derived here: it comes from
//! [`StorageClass::vanishing_is_expected`], so there is exactly one place in the
//! workspace that decides what "expected" means.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ephervex_model::{AttentionReason, InstanceId, ServerId, StorageClass};

use crate::metadata;
use crate::records::{ClientRecord, FileDisposition, ServerRecord};
use crate::registry::{
    HostProbe, Registry, RegistryError, canonical_client_at, canonical_server_at,
};

/// Which record a reconciliation entry refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordRef {
    Client(InstanceId),
    Server(ServerId),
}

impl std::fmt::Display for RecordRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Client(id) => write!(f, "{id}"),
            Self::Server(id) => write!(f, "{id}"),
        }
    }
}

/// One record reconciliation acted on.
///
/// Carries the name and root so a frontend can show which instance it is
/// talking about without a second query, and so the CLI can print it after the
/// index row has already been deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledRecord {
    pub reference: RecordRef,
    pub name: String,
    pub root: PathBuf,
    pub storage: StorageClass,
}

/// What startup reconciliation did.
///
/// Split by outcome rather than by record type because the *outcome* is what
/// determines how it is presented: silently, as a warning, or as good news.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconciliationReport {
    /// Temporary roots the OS cleaned up. Informational only.
    pub removed_stale_temporary: Vec<ReconciledRecord>,
    /// Persistent roots that are missing. Kept, warned about, never deleted.
    pub missing_persistent: Vec<ReconciledRecord>,
    /// Directories found on disk and added back to the index.
    pub reindexed: Vec<ReconciledRecord>,
    /// Records whose previously missing root came back.
    pub recovered: Vec<ReconciledRecord>,
    /// Records whose files were found at the persistent root they were being
    /// moved to, so the record followed them instead of being forgotten
    /// (ADR 0025).
    pub relocated: Vec<ReconciledRecord>,
    /// Valid canonical metadata that replaced a disagreeing SQLite copy.
    pub index_repaired: Vec<ReconciledRecord>,
    /// Hosts whose launcher-owned staging or rollback directory is not empty.
    ///
    /// The files are left untouched. This report and the persisted attention
    /// reason make the interruption visible until the user resolves it.
    pub interrupted_hosts: Vec<ReconciledRecord>,
    /// Hosts whose recognized interrupted snapshot restore was rolled back to
    /// the complete pre-restore state.
    pub restores_rolled_back: Vec<ReconciledRecord>,
    /// Indexed recovery copies whose canonical metadata is unavailable or
    /// unusable. These remain visible but must not be launched or mutated.
    pub persistence_unhealthy: Vec<ReconciledRecord>,
    /// Several valid roots claimed the same client identity. No root was
    /// selected automatically.
    pub client_conflicts: Vec<ClientRootConflict>,
    /// Several valid roots claimed the same server identity. No root was
    /// selected automatically.
    pub server_conflicts: Vec<ServerRootConflict>,
    /// Instance directories whose materialization never finished.
    ///
    /// Left exactly where they are: they hold whatever an interrupted creation
    /// or import managed to write, and only the user can say whether that is
    /// worth keeping. They are never indexed, because a half-built instance
    /// presented as a finished one is the failure this state exists to
    /// prevent.
    pub unfinished_instances: Vec<ReconciledRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRootConflict {
    pub id: InstanceId,
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRootConflict {
    pub id: ServerId,
    pub roots: Vec<PathBuf>,
}

impl ReconciliationReport {
    /// Whether the index already agreed with the filesystem.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.removed_stale_temporary.is_empty()
            && self.missing_persistent.is_empty()
            && self.reindexed.is_empty()
            && self.recovered.is_empty()
            && self.relocated.is_empty()
            && self.index_repaired.is_empty()
            && self.interrupted_hosts.is_empty()
            && self.restores_rolled_back.is_empty()
            && self.persistence_unhealthy.is_empty()
            && self.client_conflicts.is_empty()
            && self.server_conflicts.is_empty()
            && self.unfinished_instances.is_empty()
    }

    /// Whether anything here needs the user's attention.
    ///
    /// A cleared temporary root does not; a missing persistent one does.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        !self.missing_persistent.is_empty()
            || !self.interrupted_hosts.is_empty()
            || !self.persistence_unhealthy.is_empty()
            || !self.client_conflicts.is_empty()
            || !self.server_conflicts.is_empty()
            || !self.unfinished_instances.is_empty()
    }
}

/// Runs startup reconciliation.
///
/// Called once per launch, before the instance list is shown. Without it the
/// list fills with entries pointing at directories a reboot removed.
pub fn reconcile(registry: &Registry) -> Result<ReconciliationReport, RegistryError> {
    let mut report = ReconciliationReport::default();

    reconcile_clients(registry, &mut report)?;
    reconcile_servers(registry, &mut report)?;
    reindex_orphaned_clients(registry, &mut report)?;
    reindex_orphaned_servers(registry, &mut report)?;

    if report.is_empty() {
        tracing::debug!("startup reconciliation found nothing to do");
    } else {
        tracing::info!(
            removed_stale_temporary = report.removed_stale_temporary.len(),
            missing_persistent = report.missing_persistent.len(),
            reindexed = report.reindexed.len(),
            recovered = report.recovered.len(),
            relocated = report.relocated.len(),
            index_repaired = report.index_repaired.len(),
            interrupted_hosts = report.interrupted_hosts.len(),
            restores_rolled_back = report.restores_rolled_back.len(),
            persistence_unhealthy = report.persistence_unhealthy.len(),
            client_conflicts = report.client_conflicts.len(),
            server_conflicts = report.server_conflicts.len(),
            "startup reconciliation complete"
        );
    }

    Ok(report)
}

fn client_entry(record: &ClientRecord) -> ReconciledRecord {
    ReconciledRecord {
        reference: RecordRef::Client(record.id),
        name: record.name.clone(),
        root: record.root.clone(),
        storage: record.storage,
    }
}

fn server_entry(record: &ServerRecord) -> ReconciledRecord {
    ReconciledRecord {
        reference: RecordRef::Server(record.id),
        name: record.name.clone(),
        root: record.root.clone(),
        storage: record.storage,
    }
}

/// Reconciles every indexed client.
///
/// A host's claim is taken before its record is read. A restore or move another
/// process is still running owns the host's files and transaction state, and
/// one that finishes after the listing has already changed the record the
/// listing returned.
fn reconcile_clients(
    registry: &Registry,
    report: &mut ReconciliationReport,
) -> Result<(), RegistryError> {
    let listed = registry.list_clients()?;
    reconcile_listed_clients(registry, &listed, report)
}

/// Reconciles the clients of a listing taken earlier, deciding each from the
/// record read after its claim rather than from the listing.
fn reconcile_listed_clients(
    registry: &Registry,
    listed: &[ClientRecord],
    report: &mut ReconciliationReport,
) -> Result<(), RegistryError> {
    for listed in listed {
        let probe = registry.probe_host(RecordRef::Client(listed.id));
        if matches!(probe, HostProbe::Mutating) {
            continue;
        }
        let record = match registry.client(listed.id) {
            Ok(record) => record,
            Err(RegistryError::InstanceUnknown { .. }) => continue,
            Err(error) => return Err(error),
        };
        reconcile_client(
            registry,
            record,
            matches!(probe, HostProbe::Idle(_)),
            report,
        )?;
    }
    Ok(())
}

fn reconcile_client(
    registry: &Registry,
    mut record: ClientRecord,
    idle: bool,
    report: &mut ReconciliationReport,
) -> Result<(), RegistryError> {
    let entry = client_entry(&record);

    if record.storage == StorageClass::Temporary
        && record.root != registry.paths().instance_root(record.storage, record.id)
    {
        // Temporary roots are launcher-owned and UID-scoped. An older or
        // tampered record pointing elsewhere must not bypass that boundary;
        // forget it without deleting the path, which may belong to a user
        // or to another account on the machine.
        registry.delete_client(record.id, FileDisposition::Keep)?;
        report.removed_stale_temporary.push(entry);
        return Ok(());
    }

    if record.root.is_dir() {
        return reconcile_present_client(registry, record, idle, report);
    }

    if record.storage.vanishing_is_expected() {
        // Before calling it cleanup, check whether the files simply moved.
        // A promotion that was interrupted between its rename and its index
        // row leaves exactly this state, and forgetting the record would
        // also unlink its servers (ADR 0025).
        let moved = registry
            .paths()
            .instance_root(StorageClass::Persistent, record.id);
        if is_finished_client_root(&moved, record.id) {
            // The index row can be older than the files; the moved metadata
            // is the host's current state.
            record = canonical_client_at(&moved, record.id)?;
            record.root.clone_from(&moved);
            record.storage = StorageClass::Persistent;
            registry.update_client(&mut record)?;
            tracing::info!(
                instance_id = %record.id,
                root = %moved.display(),
                "an instance's files were found at the persistent root; the record followed them"
            );
            report.relocated.push(client_entry(&record));
            return reconcile_present_client(registry, record, idle, report);
        }

        // Expected: the OS owns this directory's lifecycle (ADR 0006).
        // `Keep` because there is nothing left to remove, and because
        // asking the filesystem to delete a path the OS already cleared is
        // how a race turns into an error dialog.
        registry.delete_client(record.id, FileDisposition::Keep)?;
        tracing::debug!(
            instance_id = %record.id,
            root = %record.root.display(),
            "dropped a stale temporary instance whose directory the OS cleared"
        );
        report.removed_stale_temporary.push(entry);
    } else {
        // Not expected. Keep everything; say so.
        if !record.attention.contains(&AttentionReason::MissingRoot) {
            record.add_attention(AttentionReason::MissingRoot);
            registry.update_client(&mut record)?;
        }
        tracing::warn!(
            instance_id = %record.id,
            root = %record.root.display(),
            "a persistent instance is missing its directory; keeping the record"
        );
        report.missing_persistent.push(entry);
    }
    Ok(())
}

/// Reconciles a client whose files are standing at `record.root`.
///
/// Canonical metadata wins over the index: ADR 0002 writes it first, so a
/// disagreeing row is the leftover of an update whose index write failed. Two
/// cases keep the index instead. While a root was absent only the index could
/// be written, so a newer row carrying `MissingRoot` is the later state. And a
/// root with an unresolved restore holds a snapshot's copy in place of
/// canonical metadata, which is never adopted.
fn reconcile_present_client(
    registry: &Registry,
    mut record: ClientRecord,
    idle: bool,
    report: &mut ReconciliationReport,
) -> Result<(), RegistryError> {
    let entry = client_entry(&record);
    let recovery = if idle {
        recover_restore_state(&record.root, &entry, report)
    } else {
        RestoreRecovery::Unknown
    };
    let restore_unresolved = matches!(recovery, RestoreRecovery::Failed | RestoreRecovery::Unknown)
        || metadata::is_incomplete(&record.root);

    let mut adopted = false;
    match metadata::read_client_metadata(&record.root) {
        Ok(mut canonical) if canonical.id == record.id => {
            if restore_unresolved {
                if metadata::is_incomplete(&record.root) {
                    report.persistence_unhealthy.push(entry);
                }
            } else if canonical.storage == StorageClass::Temporary
                && record.root
                    != registry
                        .paths()
                        .instance_root(StorageClass::Temporary, record.id)
            {
                // A storage class that contradicts where the files are would
                // make the next pass forget the record.
                report.persistence_unhealthy.push(entry);
                return Ok(());
            } else {
                let recorded_root_differs = canonical.root != record.root;
                canonical.root.clone_from(&record.root);
                let index_is_newer = record.attention.contains(&AttentionReason::MissingRoot)
                    && record.updated_at > canonical.updated_at;
                if index_is_newer {
                    adopted = true;
                } else {
                    if record.attention.contains(&AttentionReason::MissingRoot) {
                        canonical.add_attention(AttentionReason::MissingRoot);
                    }
                    if canonical != record {
                        if recorded_root_differs {
                            metadata::write_client_metadata(&record.root, &canonical)?;
                        }
                        registry.synchronize_client_index(&canonical)?;
                        report.index_repaired.push(client_entry(&canonical));
                    }
                    record = canonical;
                    adopted = true;
                }
            }
        }
        Ok(_) | Err(_) => {
            tracing::warn!(
                instance_id = %record.id,
                root = %record.root.display(),
                "client canonical metadata is unhealthy; retaining only the recovery copy"
            );
            report.persistence_unhealthy.push(entry);
            return Ok(());
        }
    }

    let mut changed = false;
    // The disk came back. Clearing the warning here is what stops a
    // remounted drive from leaving a permanent "needs attention" badge.
    if record.attention.contains(&AttentionReason::MissingRoot) {
        record
            .attention
            .retain(|reason| reason != &AttentionReason::MissingRoot);
        changed = true;
        report.recovered.push(client_entry(&record));
    }
    let entry = client_entry(&record);
    changed |= report_interrupted_state(&record.root, &mut record.attention, &entry, report);
    if changed {
        if adopted {
            registry.update_client(&mut record)?;
        } else {
            // The files under this root are not canonical metadata, so only
            // the index records the new attention.
            registry.synchronize_client_index(&record)?;
        }
    }
    Ok(())
}

/// Reconciles every indexed server, claim first like [`reconcile_clients`].
fn reconcile_servers(
    registry: &Registry,
    report: &mut ReconciliationReport,
) -> Result<(), RegistryError> {
    let listed = registry.list_servers()?;
    reconcile_listed_servers(registry, &listed, report)
}

fn reconcile_listed_servers(
    registry: &Registry,
    listed: &[ServerRecord],
    report: &mut ReconciliationReport,
) -> Result<(), RegistryError> {
    for listed in listed {
        let probe = registry.probe_host(RecordRef::Server(listed.id));
        if matches!(probe, HostProbe::Mutating) {
            continue;
        }
        let record = match registry.server(listed.id) {
            Ok(record) => record,
            Err(RegistryError::ServerUnknown { .. }) => continue,
            Err(error) => return Err(error),
        };
        reconcile_server(
            registry,
            record,
            matches!(probe, HostProbe::Idle(_)),
            report,
        )?;
    }
    Ok(())
}

fn reconcile_server(
    registry: &Registry,
    mut record: ServerRecord,
    idle: bool,
    report: &mut ReconciliationReport,
) -> Result<(), RegistryError> {
    let entry = server_entry(&record);

    if record.storage == StorageClass::Temporary
        && record.root != registry.paths().server_root(record.storage, record.id)
    {
        registry.delete_server(record.id, FileDisposition::Keep)?;
        report.removed_stale_temporary.push(entry);
        return Ok(());
    }

    if record.root.is_dir() {
        return reconcile_present_server(registry, record, idle, report);
    }

    if record.storage.vanishing_is_expected() {
        // The same question a client is asked: were these files moved to
        // the persistent root rather than cleared away (ADR 0025)?
        let moved = registry
            .paths()
            .server_root(StorageClass::Persistent, record.id);
        if is_finished_server_root(&moved, record.id) {
            record = canonical_server_at(&moved, record.id)?;
            record.root.clone_from(&moved);
            record.storage = StorageClass::Persistent;
            let _committed = registry.update_server_committed(&mut record)?;
            tracing::info!(
                server_id = %record.id,
                root = %moved.display(),
                "a server's files were found at the persistent root; the record followed them"
            );
            report.relocated.push(server_entry(&record));
            return reconcile_present_server(registry, record, idle, report);
        }

        registry.delete_server(record.id, FileDisposition::Keep)?;
        report.removed_stale_temporary.push(entry);
    } else {
        if !record.attention.contains(&AttentionReason::MissingRoot) {
            record.add_attention(AttentionReason::MissingRoot);
            registry.update_server(&mut record)?;
        }
        tracing::warn!(
            server_id = %record.id,
            root = %record.root.display(),
            "a persistent server is missing its directory; keeping the record"
        );
        report.missing_persistent.push(entry);
    }
    Ok(())
}

/// Reconciles a server whose files are standing at `record.root`.
///
/// Servers follow the same rules as [`reconcile_present_client`]: canonical
/// metadata wins over the index, except that a newer `MissingRoot` row is the
/// later state and a root with an unresolved restore holds a snapshot's copy,
/// which is never adopted.
fn reconcile_present_server(
    registry: &Registry,
    mut record: ServerRecord,
    idle: bool,
    report: &mut ReconciliationReport,
) -> Result<(), RegistryError> {
    let entry = server_entry(&record);
    let recovery = if idle {
        recover_restore_state(&record.root, &entry, report)
    } else {
        RestoreRecovery::Unknown
    };
    let restore_unresolved = matches!(recovery, RestoreRecovery::Failed | RestoreRecovery::Unknown)
        || metadata::is_incomplete(&record.root);

    let mut adopted = false;
    match metadata::read_server_metadata(&record.root) {
        Ok(mut canonical) if canonical.id == record.id => {
            if restore_unresolved {
                if metadata::is_incomplete(&record.root) {
                    report.persistence_unhealthy.push(entry);
                }
            } else if canonical.storage == StorageClass::Temporary
                && record.root
                    != registry
                        .paths()
                        .server_root(StorageClass::Temporary, record.id)
            {
                // A storage class that contradicts where the files are would
                // make the next pass forget the record.
                report.persistence_unhealthy.push(entry);
                return Ok(());
            } else {
                let recorded_root_differs = canonical.root != record.root;
                canonical.root.clone_from(&record.root);
                let index_is_newer = record.attention.contains(&AttentionReason::MissingRoot)
                    && record.updated_at > canonical.updated_at;
                if index_is_newer {
                    adopted = true;
                } else {
                    if record.attention.contains(&AttentionReason::MissingRoot) {
                        canonical.add_attention(AttentionReason::MissingRoot);
                    }
                    if canonical != record {
                        if recorded_root_differs {
                            metadata::write_server_metadata(&record.root, &canonical)?;
                        }
                        registry.synchronize_server_index(&canonical)?;
                        report.index_repaired.push(server_entry(&canonical));
                    }
                    record = canonical;
                    adopted = true;
                }
            }
        }
        Ok(_) | Err(_) => {
            tracing::warn!(
                server_id = %record.id,
                root = %record.root.display(),
                "server canonical metadata is unhealthy; retaining only the recovery copy"
            );
            report.persistence_unhealthy.push(entry);
            return Ok(());
        }
    }

    let mut changed = false;
    if record.attention.contains(&AttentionReason::MissingRoot) {
        record
            .attention
            .retain(|reason| reason != &AttentionReason::MissingRoot);
        changed = true;
        report.recovered.push(server_entry(&record));
    }
    let entry = server_entry(&record);
    changed |= report_interrupted_state(&record.root, &mut record.attention, &entry, report);
    if changed {
        if adopted {
            registry.update_server(&mut record)?;
        } else {
            // The files under this root are not canonical metadata, so only
            // the index records the new attention.
            registry.synchronize_server_index(&record)?;
        }
    }
    Ok(())
}

/// Scans the persistent instance root for instances the index has lost.
///
/// Only the persistent root is scanned. A temporary directory the index does
/// not know about is indistinguishable from one the OS is about to delete, and
/// re-indexing it would resurrect exactly the entries the previous step exists
/// to remove (step 5).
fn reindex_orphaned_clients(
    registry: &Registry,
    report: &mut ReconciliationReport,
) -> Result<(), RegistryError> {
    let indexed = registry.list_clients()?;
    let mut discovered =
        std::collections::BTreeMap::<InstanceId, Vec<(PathBuf, ClientRecord)>>::new();

    for directory in candidate_directories(&registry.paths().persistent_instances_dir()) {
        // A move's copy that was never renamed into place is part of a host at
        // best, never adopted, and may not hold its metadata yet.
        if let Some(staged) = crate::relocate::staged_name(&directory) {
            if let Ok(id) = staged.parse::<InstanceId>()
                && !matches!(
                    registry.probe_host(RecordRef::Client(id)),
                    HostProbe::Mutating
                )
            {
                let name = metadata::read_client_metadata(&directory)
                    .map_or_else(|_| staged.to_owned(), |record| record.name);
                report_unfinished_move(report, RecordRef::Client(id), name, directory);
            }
            continue;
        }
        if !metadata::is_client_instance_root(&directory) {
            continue;
        }

        let record = match metadata::read_client_metadata(&directory) {
            Ok(record) => record,
            Err(error) => {
                // A directory that looks like an instance but does not parse is
                // reported, not repaired and not deleted.
                tracing::warn!(
                    path = %directory.display(),
                    %error,
                    "ignoring an unreadable instance metadata file during reconciliation"
                );
                continue;
            }
        };

        let entry = indexed.iter().find(|entry| entry.id == record.id);
        let Some(record) =
            settle_unfinished_client_root(registry, &directory, record, entry, report)
        else {
            continue;
        };

        discovered
            .entry(record.id)
            .or_default()
            .push((directory, record));
    }

    for (id, mut roots) in discovered {
        roots.sort_by(|left, right| left.0.cmp(&right.0));
        roots.dedup_by(|left, right| left.0 == right.0);
        if roots.len() > 1 {
            report.client_conflicts.push(ClientRootConflict {
                id,
                roots: roots.into_iter().map(|(directory, _)| directory).collect(),
            });
            continue;
        }
        let (directory, mut record) = roots.remove(0);

        if let Some(entry) = indexed.iter().find(|entry| entry.id == id) {
            // A copy cannot be told apart from an indexed original that is
            // still in place. A held claim is a move mid-flight, not a copy.
            if entry.root != directory
                && entry.root.is_dir()
                && !matches!(
                    registry.probe_host(RecordRef::Client(id)),
                    HostProbe::Mutating
                )
            {
                report.client_conflicts.push(ClientRootConflict {
                    id,
                    roots: vec![entry.root.clone(), directory],
                });
            }
            continue;
        }

        // Trust where the directory actually is over where the metadata says it
        // was: instances get moved, and the file travels with them.
        record.root.clone_from(&directory);
        record.storage = StorageClass::Persistent;

        registry.index_existing_client(&record)?;
        tracing::info!(
            instance_id = %record.id,
            root = %directory.display(),
            "re-indexed an instance found on disk but missing from the index"
        );
        report.reindexed.push(ReconciledRecord {
            reference: RecordRef::Client(record.id),
            name: record.name,
            root: directory,
            storage: StorageClass::Persistent,
        });
    }
    Ok(())
}

/// Decides whether a client directory read from the persistent root can be
/// considered for indexing, returning its metadata if so.
///
/// `indexed` is the index row for the directory's id, if any. A directory that
/// is skipped or reported is never indexed.
fn settle_unfinished_client_root(
    registry: &Registry,
    directory: &Path,
    record: ClientRecord,
    indexed: Option<&ClientRecord>,
    report: &mut ReconciliationReport,
) -> Option<ClientRecord> {
    let marked = metadata::is_incomplete(directory);

    // The pass over the index already reconciled this root, recovering its
    // restore or reporting it, so a marker here is not an unfinished creation.
    if marked && indexed.is_some_and(|entry| entry.root == directory) {
        return None;
    }

    // The marker is checked before the index, not after it. An interrupted
    // duplication carries the *original's* `instance.json` until the copy
    // finishes, so its id is already indexed; testing the index first would
    // skip the one directory the marker was written for.
    //
    // An instance whose creation, import, or duplication never finished
    // parses perfectly and is missing whatever the materialization had not
    // written yet. Adopting it would present a half-built instance as a
    // complete one, which is exactly what the marker exists to prevent.
    // Reported and left alone -- never indexed, and never deleted on the
    // user's behalf.
    if marked && indexed.is_some() {
        tracing::warn!(
            instance_id = %record.id,
            path = %directory.display(),
            "an instance directory was never finished; leaving it unindexed"
        );
        report.unfinished_instances.push(ReconciledRecord {
            reference: RecordRef::Client(record.id),
            name: record.name,
            root: directory.to_path_buf(),
            storage: StorageClass::Persistent,
        });
        return None;
    }

    // A root the index does not know may be a restore's. The marker is also an
    // interrupted creation's, so recovery is only the first step: the root is
    // reported unless nothing is left unresolved afterward.
    if indexed.is_none() && (marked || has_interrupted_state(directory)) {
        // A restore still running holds the host's claim and owns the files; one
        // that was abandoned is recovered before anything is read from them.
        let probe = registry.probe_host(RecordRef::Client(record.id));
        if matches!(probe, HostProbe::Mutating) {
            return None;
        }
        return resolve_orphaned_client(
            record.id,
            directory,
            record.name,
            matches!(probe, HostProbe::Idle(_)),
            report,
        );
    }
    Some(record)
}

fn reindex_orphaned_servers(
    registry: &Registry,
    report: &mut ReconciliationReport,
) -> Result<(), RegistryError> {
    let indexed = registry.list_servers()?;
    let known: HashSet<ServerId> = indexed.iter().map(|record| record.id).collect();

    let mut discovered = std::collections::BTreeMap::<ServerId, Vec<PathBuf>>::new();
    for directory in candidate_directories(&registry.paths().persistent_servers_dir()) {
        if let Some(staged) = crate::relocate::staged_name(&directory) {
            if let Ok(id) = staged.parse::<ServerId>()
                && !matches!(
                    registry.probe_host(RecordRef::Server(id)),
                    HostProbe::Mutating
                )
            {
                let name = metadata::read_server_metadata(&directory)
                    .map_or_else(|_| staged.to_owned(), |record| record.name);
                report_unfinished_move(report, RecordRef::Server(id), name, directory);
            }
            continue;
        }
        if !metadata::is_server_root(&directory) {
            continue;
        }
        if let Ok(record) = metadata::read_server_metadata(&directory) {
            discovered.entry(record.id).or_default().push(directory);
        }
    }

    for (id, mut roots) in discovered {
        roots.sort();
        roots.dedup();
        if roots.len() > 1 {
            report
                .server_conflicts
                .push(ServerRootConflict { id, roots });
            continue;
        }
        let directory = roots.remove(0);

        // An indexed root was reconciled, with its restore recovered, by the
        // pass over the index.
        if indexed
            .iter()
            .any(|record| record.id == id && record.root == directory)
        {
            continue;
        }

        // The host's claim is held until this root is dealt with. A restore or
        // move still running owns the files, and one that was abandoned is
        // recovered before anything is read from them.
        let probe = registry.probe_host(RecordRef::Server(id));
        if matches!(probe, HostProbe::Mutating) {
            continue;
        }
        let Some(mut record) =
            resolve_orphaned_server(id, &directory, matches!(probe, HostProbe::Idle(_)), report)
        else {
            continue;
        };

        // Trust the directory that actually contains the metadata. Correct the
        // canonical file atomically before mirroring the index.
        if record.root != directory || record.storage != StorageClass::Persistent {
            record.root.clone_from(&directory);
            record.storage = StorageClass::Persistent;
            metadata::write_server_metadata(&directory, &record)?;
        }

        if known.contains(&record.id) {
            let indexed = registry.server(record.id)?;
            if indexed.root != directory && indexed.root.is_dir() {
                report.server_conflicts.push(ServerRootConflict {
                    id: record.id,
                    roots: vec![indexed.root, directory],
                });
                continue;
            }
            if indexed != record {
                registry.synchronize_server_index(&record)?;
                report.index_repaired.push(ReconciledRecord {
                    reference: RecordRef::Server(record.id),
                    name: record.name,
                    root: record.root,
                    storage: record.storage,
                });
            }
            continue;
        }

        // A re-indexed server may reference a client that is no longer indexed.
        // Clear it through canonical metadata before the SQLite mirror.
        if let Some(client) = record.linked_client_id
            && registry.client(client).is_err()
        {
            record.linked_client_id = None;
            metadata::write_server_metadata(&directory, &record)?;
        }

        registry.index_existing_server(&record)?;
        report.reindexed.push(ReconciledRecord {
            reference: RecordRef::Server(record.id),
            name: record.name,
            root: directory,
            storage: StorageClass::Persistent,
        });
    }

    Ok(())
}

/// Recovers the restore of a server directory the index has lost and returns
/// its canonical metadata, or reports the directory and returns nothing.
///
/// Recovery rewrites canonical metadata, so it is read again afterward. A root
/// whose restore is unresolved holds a snapshot's copy and possibly
/// half-replaced contents. It is reported and left unindexed, and the index is
/// never repaired from it.
fn resolve_orphaned_server(
    id: ServerId,
    directory: &Path,
    idle: bool,
    report: &mut ReconciliationReport,
) -> Option<ServerRecord> {
    let discovered = metadata::read_server_metadata(directory).ok()?;
    let entry = ReconciledRecord {
        reference: RecordRef::Server(id),
        name: discovered.name,
        root: directory.to_path_buf(),
        storage: StorageClass::Persistent,
    };
    let recovery = recover_orphaned_restore(directory, idle, &entry, report);

    let Some(record) = metadata::read_server_metadata(directory)
        .ok()
        .filter(|record| record.id == id)
    else {
        report.persistence_unhealthy.push(entry);
        return None;
    };
    if matches!(recovery, RestoreRecovery::Failed | RestoreRecovery::Unknown)
        || metadata::is_incomplete(directory)
    {
        tracing::warn!(
            server_id = %id,
            path = %directory.display(),
            "a server directory has an unresolved restore or was never finished; leaving it unindexed"
        );
        report.unfinished_instances.push(ReconciledRecord {
            name: record.name,
            ..entry
        });
        return None;
    }
    Some(record)
}

/// Recovers the restore of a host root the index has lost, when its claim is
/// held by the caller (`idle`). Without the claim a restore cannot be told
/// from an abandoned one, so any restore state is left alone and reported as
/// unknown.
fn recover_orphaned_restore(
    root: &Path,
    idle: bool,
    entry: &ReconciledRecord,
    report: &mut ReconciliationReport,
) -> RestoreRecovery {
    if idle {
        recover_restore_state(root, entry, report)
    } else if metadata::is_incomplete(root) || has_interrupted_state(root) {
        RestoreRecovery::Unknown
    } else {
        // The claim could not be tried, but there is no restore to recover.
        RestoreRecovery::Nothing
    }
}

/// Recovers the restore of a client directory the index has lost and returns
/// its canonical metadata, or reports the directory and returns nothing.
///
/// Like [`resolve_orphaned_server`], recovery rewrites canonical metadata, so
/// it is read again afterward, and a root whose restore is unresolved is
/// reported and left unindexed.
fn resolve_orphaned_client(
    id: InstanceId,
    directory: &Path,
    name: String,
    idle: bool,
    report: &mut ReconciliationReport,
) -> Option<ClientRecord> {
    let entry = ReconciledRecord {
        reference: RecordRef::Client(id),
        name,
        root: directory.to_path_buf(),
        storage: StorageClass::Persistent,
    };
    let recovery = recover_orphaned_restore(directory, idle, &entry, report);

    let Some(record) = metadata::read_client_metadata(directory)
        .ok()
        .filter(|record| record.id == id)
    else {
        report.persistence_unhealthy.push(entry);
        return None;
    };
    if matches!(recovery, RestoreRecovery::Failed | RestoreRecovery::Unknown)
        || metadata::is_incomplete(directory)
    {
        tracing::warn!(
            instance_id = %id,
            path = %directory.display(),
            "an instance directory has an unresolved restore or was never finished; leaving it unindexed"
        );
        report.unfinished_instances.push(ReconciledRecord {
            name: record.name,
            ..entry
        });
        return None;
    }
    Some(record)
}

/// Whether a finished client instance with this id is standing at `root`.
///
/// All three conditions matter. The directory has to look like an instance, its
/// metadata has to name this instance rather than another one, and it must not
/// carry the incomplete marker, which no finished move leaves behind (ADR 0025).
fn is_finished_client_root(root: &Path, id: InstanceId) -> bool {
    metadata::is_client_instance_root(root)
        && !metadata::is_incomplete(root)
        && metadata::read_client_metadata(root).is_ok_and(|found| found.id == id)
}

/// The same three conditions for a dedicated server.
fn is_finished_server_root(root: &Path, id: ServerId) -> bool {
    metadata::is_server_root(root)
        && !metadata::is_incomplete(root)
        && metadata::read_server_metadata(root).is_ok_and(|found| found.id == id)
}

fn report_unfinished_move(
    report: &mut ReconciliationReport,
    reference: RecordRef,
    name: String,
    root: PathBuf,
) {
    tracing::warn!(
        path = %root.display(),
        "a move's copy was never put in place; leaving it unindexed"
    );
    report.unfinished_instances.push(ReconciledRecord {
        reference,
        name,
        root,
        storage: StorageClass::Persistent,
    });
}

/// Immediate subdirectories of `root`, or nothing if it does not exist.
///
/// A missing persistent instance root is a first run or a not-yet-mounted disk,
/// not an error worth failing startup over.
fn candidate_directories(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };

    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

/// Whether launcher-owned transaction state needs a person's attention.
///
/// An unreadable path, a non-directory at the reserved name, or any directory
/// entry counts. Reconciliation must fail visible rather than assume an
/// unfamiliar shape is disposable.
fn has_interrupted_state(root: &Path) -> bool {
    [metadata::retired_dir(root), metadata::staging_dir(root)]
        .into_iter()
        .any(|path| match std::fs::symlink_metadata(&path) {
            Ok(file_type) if file_type.is_dir() => {
                std::fs::read_dir(path).map_or(true, |mut entries| entries.next().is_some())
            }
            Ok(_) => true,
            Err(error) => error.kind() != std::io::ErrorKind::NotFound,
        })
}

fn report_interrupted_state(
    root: &Path,
    attention: &mut Vec<AttentionReason>,
    record: &ReconciledRecord,
    report: &mut ReconciliationReport,
) -> bool {
    if !has_interrupted_state(root) {
        return false;
    }

    report.interrupted_hosts.push(record.clone());
    if attention.contains(&AttentionReason::InterruptedMutation) {
        false
    } else {
        attention.push(AttentionReason::InterruptedMutation);
        true
    }
}

/// What trying to recover a host's interrupted restore found.
enum RestoreRecovery {
    /// No restore was interrupted.
    Nothing,
    /// A restore was rolled back to the complete pre-restore state.
    RolledBack,
    /// A restore is interrupted and could not be rolled back.
    Failed,
    /// Recovery was not attempted because the host's claim was not free.
    Unknown,
}

fn recover_restore_state(
    root: &Path,
    record: &ReconciledRecord,
    report: &mut ReconciliationReport,
) -> RestoreRecovery {
    match crate::restore::recover_interrupted_restores(root) {
        Ok(0) => RestoreRecovery::Nothing,
        Ok(_) => {
            report.restores_rolled_back.push(record.clone());
            RestoreRecovery::RolledBack
        }
        Err(error) => {
            tracing::warn!(
                root = %root.display(),
                %error,
                "an interrupted restore could not be reconciled automatically"
            );
            RestoreRecovery::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use chrono::Utc;
    use ephervex_model::LoaderSelection;

    use crate::records::{NewClient, NewServer};
    use crate::test_support::TestEnvironment;

    #[test]
    fn a_vanished_temporary_root_is_removed_silently_and_a_persistent_one_is_not() {
        // The single most important behavior in this module.
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

        // Simulate the OS clearing its temporary directory, and a disk that
        // failed to mount.
        std::fs::remove_dir_all(&temporary.root).expect("clear temp");
        std::fs::remove_dir_all(&persistent.root).expect("unmount");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.removed_stale_temporary.len(), 1);
        assert_eq!(
            report.removed_stale_temporary[0].reference,
            RecordRef::Client(temporary.id)
        );
        assert!(matches!(
            registry.client(temporary.id),
            Err(RegistryError::InstanceUnknown { .. })
        ));

        assert_eq!(report.missing_persistent.len(), 1);
        let kept = registry
            .client(persistent.id)
            .expect("persistent record is kept");
        assert!(kept.attention.contains(&AttentionReason::MissingRoot));
        assert!(report.needs_attention());
    }

    /// A promotion interrupted between moving the files and recording where
    /// they went. Without this rule the record is dropped as expected cleanup,
    /// which also unlinks every server attached to it (ADR 0025).
    #[test]
    fn a_temporary_record_whose_files_moved_to_the_persistent_root_is_relocated_rather_than_forgotten()
     {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new(
                "repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        let server = registry
            .create_server(NewServer::linked("repro server", client.id))
            .expect("create server");

        // The rename happened; the index row did not.
        let moved = registry
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        std::fs::create_dir_all(moved.parent().expect("parent")).expect("mkdir");
        std::fs::rename(&client.root, &moved).expect("move the files");

        let report = reconcile(&registry).expect("reconcile");

        assert!(
            report.removed_stale_temporary.is_empty(),
            "the files are right there; nothing was cleaned up"
        );
        assert_eq!(report.relocated.len(), 1);
        assert_eq!(report.relocated[0].reference, RecordRef::Client(client.id));

        let followed = registry.client(client.id).expect("the record survives");
        assert_eq!(followed.storage, StorageClass::Persistent);
        assert_eq!(followed.root, moved);
        assert_eq!(
            registry.linked_server_ids(client.id).expect("links"),
            vec![server.id],
            "the linked server is still attached, which is the whole point"
        );
        assert!(!report.needs_attention());
    }

    /// An interrupted copy leaves only its staging directory. Adopting it would
    /// hand the user a half-copied instance.
    #[test]
    fn an_interrupted_copy_is_reported_and_never_adopted() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new(
                "repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        for name in ["one.jar", "two.jar", "three.jar"] {
            std::fs::write(client.root.join("minecraft").join(name), name).expect("payload");
        }
        let destination = registry
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        let all = crate::test_support::count_files(&client.root);
        crate::test_support::interrupt_copy_after(&client.root, &destination, all);
        std::fs::remove_dir_all(&client.root).expect("clear temp");

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.relocated.is_empty());
        assert!(report.reindexed.is_empty());
        assert_eq!(report.removed_stale_temporary.len(), 1);
        assert_eq!(report.unfinished_instances.len(), 1);
        assert!(registry.list_clients().expect("list").is_empty());
    }

    #[test]
    fn an_interrupted_server_copy_is_reported_and_never_adopted() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new(
                "repro server",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        for name in ["one.jar", "two.jar", "three.jar"] {
            std::fs::write(server.root.join("server").join(name), name).expect("payload");
        }
        let destination = registry
            .paths()
            .server_root(StorageClass::Persistent, server.id);
        let all = crate::test_support::count_files(&server.root);
        crate::test_support::interrupt_copy_after(&server.root, &destination, all);
        std::fs::remove_dir_all(&server.root).expect("clear temp");

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.relocated.is_empty());
        assert!(report.reindexed.is_empty());
        assert!(report.server_conflicts.is_empty());
        assert_eq!(report.unfinished_instances.len(), 1);
        assert!(registry.server(server.id).is_err());
    }

    /// A marked root is not a finished move; its marker is a restore's or a
    /// creation's, and neither is a complete host to follow.
    #[test]
    fn a_marked_root_at_the_persistent_path_is_not_followed() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new(
                "repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        crate::metadata::mark_incomplete(&client.root).expect("a marker");
        let moved = registry
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        std::fs::create_dir_all(moved.parent().expect("parent")).expect("mkdir");
        std::fs::rename(&client.root, &moved).expect("move the files");

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.relocated.is_empty());
        assert_eq!(report.removed_stale_temporary.len(), 1);
        assert_eq!(report.unfinished_instances.len(), 1);
    }

    #[test]
    fn a_temporary_server_whose_files_moved_to_the_persistent_root_follows_them() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new(
                "repro server",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");

        let moved = registry
            .paths()
            .server_root(StorageClass::Persistent, server.id);
        std::fs::create_dir_all(moved.parent().expect("parent")).expect("mkdir");
        std::fs::rename(&server.root, &moved).expect("move the files");

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.removed_stale_temporary.is_empty());
        assert_eq!(report.relocated.len(), 1);
        let followed = registry.server(server.id).expect("the record survives");
        assert_eq!(followed.storage, StorageClass::Persistent);
        assert_eq!(followed.root, moved);
    }

    #[test]
    fn a_relocated_client_keeps_the_canonical_metadata_the_index_row_lags_behind() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new(
                "indexed",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        let mut canonical = client.clone();
        canonical.name = "canonical".to_owned();
        metadata::write_client_metadata(&client.root, &canonical).expect("canonical write");
        let moved = registry
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        std::fs::create_dir_all(moved.parent().expect("parent")).expect("mkdir");
        std::fs::rename(&client.root, &moved).expect("move the files");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.relocated.len(), 1);
        let followed = registry.client(client.id).expect("record");
        assert_eq!(followed.name, "canonical");
        assert_eq!(followed.root, moved);
        let on_disk = metadata::read_client_metadata(&moved).expect("metadata");
        assert_eq!(on_disk.name, "canonical");
        assert_eq!(on_disk.storage, StorageClass::Persistent);
    }

    #[test]
    fn a_relocated_server_keeps_the_canonical_metadata_the_index_row_lags_behind() {
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
        canonical.add_attention(AttentionReason::BestEffortServerConversion);
        metadata::write_server_metadata(&server.root, &canonical).expect("canonical write");
        let moved = registry
            .paths()
            .server_root(StorageClass::Persistent, server.id);
        std::fs::create_dir_all(moved.parent().expect("parent")).expect("mkdir");
        std::fs::rename(&server.root, &moved).expect("move the files");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.relocated.len(), 1);
        let followed = registry.server(server.id).expect("record");
        assert_eq!(followed.name, "canonical");
        assert_eq!(followed.root, moved);
        let on_disk = metadata::read_server_metadata(&moved).expect("metadata");
        assert_eq!(on_disk.name, "canonical");
        assert!(
            on_disk
                .attention
                .contains(&AttentionReason::BestEffortServerConversion)
        );
        assert_eq!(on_disk.storage, StorageClass::Persistent);
    }

    #[test]
    fn temporary_records_outside_the_private_root_are_forgotten_without_deleting_files() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let mut client = registry
            .create_published_client(NewClient::new(
                "legacy",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        let original = client.root.clone();
        let legacy = environment.base().join("data/legacy-temporary-client");
        std::fs::rename(&original, &legacy).expect("move legacy root");
        client.root.clone_from(&legacy);
        registry
            .update_client(&mut client)
            .expect("store legacy path");

        let report = reconcile(&registry).expect("reconcile");
        assert_eq!(report.removed_stale_temporary.len(), 1);
        assert!(
            legacy.is_dir(),
            "reconciliation must not delete an unowned root"
        );
        assert!(matches!(
            registry.client(client.id),
            Err(RegistryError::InstanceUnknown { .. })
        ));
    }

    #[test]
    fn only_temporary_records_are_dropped_when_both_kinds_vanish_together() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();

        for _ in 0..3 {
            registry
                .create_published_client(NewClient::new(
                    "temp",
                    "1.20.1",
                    LoaderSelection::vanilla(),
                ))
                .expect("create");
        }
        registry
            .create_published_client(
                NewClient::new("kept", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");

        std::fs::remove_dir_all(registry.paths().temporary_instances_dir()).expect("clear temp");

        let report = reconcile(&registry).expect("reconcile");
        assert_eq!(report.removed_stale_temporary.len(), 3);
        assert!(report.missing_persistent.is_empty());
        assert_eq!(registry.list_clients().expect("list").len(), 1);
    }

    #[test]
    fn reconciliation_is_idempotent() {
        // It runs on every launch; the second run must find nothing.
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let temporary = registry
            .create_published_client(NewClient::new("temp", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        std::fs::remove_dir_all(&temporary.root).expect("clear temp");

        assert!(!reconcile(&registry).expect("first").is_empty());
        assert!(reconcile(&registry).expect("second").is_empty());
    }

    #[test]
    fn interrupted_client_mutation_is_reported_persisted_and_left_untouched() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new(
                "interrupted client",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        let retired_file = metadata::retired_dir(&client.root).join("mods/example.jar");
        std::fs::create_dir_all(retired_file.parent().expect("parent")).expect("create retired");
        std::fs::write(&retired_file, b"original mod").expect("write retired file");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(
            report
                .interrupted_hosts
                .iter()
                .map(|record| record.reference)
                .collect::<Vec<_>>(),
            vec![RecordRef::Client(client.id)]
        );
        assert!(report.needs_attention());
        assert_eq!(
            std::fs::read(&retired_file).expect("retired file remains"),
            b"original mod"
        );
        assert!(
            registry
                .client(client.id)
                .expect("client")
                .attention
                .contains(&AttentionReason::InterruptedMutation)
        );

        let restarted = environment.registry();
        let after_restart = reconcile(&restarted).expect("reconcile after restart");
        assert_eq!(after_restart.interrupted_hosts.len(), 1);
        assert_eq!(
            std::fs::read(&retired_file).expect("retired file remains after restart"),
            b"original mod"
        );
    }

    #[test]
    fn interrupted_server_mutation_is_reported_without_deleting_staging() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new(
                "interrupted server",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        let staged_file = metadata::staging_dir(&server.root).join("pending.jar");
        std::fs::create_dir_all(staged_file.parent().expect("parent")).expect("create staging");
        std::fs::write(&staged_file, b"pending mod").expect("write staged file");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.interrupted_hosts.len(), 1);
        assert_eq!(
            report.interrupted_hosts[0].reference,
            RecordRef::Server(server.id)
        );
        assert_eq!(
            std::fs::read(&staged_file).expect("staged file remains"),
            b"pending mod"
        );
        assert!(
            registry
                .server(server.id)
                .expect("server")
                .attention
                .contains(&AttentionReason::InterruptedMutation)
        );
    }

    #[test]
    fn empty_transaction_directories_do_not_need_attention() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new(
                "clean client",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        std::fs::create_dir_all(metadata::retired_dir(&client.root)).expect("create retired");
        std::fs::create_dir_all(metadata::staging_dir(&client.root)).expect("create staging");

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.interrupted_hosts.is_empty());
        assert!(!report.needs_attention());
    }

    #[test]
    fn a_persistent_instance_missing_from_the_index_is_re_indexed_from_its_metadata() {
        // ADR 0002's payoff: losing the database costs an index rebuild.
        let environment = TestEnvironment::new();
        let record = {
            let registry = environment.registry();
            registry
                .create_published_client(
                    NewClient::new("recovered", "1.12.2", LoaderSelection::vanilla())
                        .with_storage(StorageClass::Persistent),
                )
                .expect("create")
        };

        // Throw the database away, exactly as a corrupt shutdown would.
        environment.destroy_database();
        let registry = environment.registry();
        assert!(registry.list_clients().expect("list").is_empty());

        let report = reconcile(&registry).expect("reconcile");
        assert_eq!(report.reindexed.len(), 1);
        assert_eq!(registry.client(record.id).expect("recovered"), record);
    }

    #[test]
    fn a_temporary_instance_missing_from_the_index_is_not_resurrected() {
        // Re-indexing the temporary root would undo the removal step above.
        let environment = TestEnvironment::new();
        {
            let registry = environment.registry();
            registry
                .create_published_client(NewClient::new(
                    "temp",
                    "1.20.1",
                    LoaderSelection::vanilla(),
                ))
                .expect("create");
        }

        environment.destroy_database();
        let registry = environment.registry();
        let report = reconcile(&registry).expect("reconcile");

        assert!(report.reindexed.is_empty());
        assert!(registry.list_clients().expect("list").is_empty());
    }

    #[test]
    fn a_directory_without_launcher_metadata_is_never_adopted() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let stray = registry
            .paths()
            .persistent_instances_dir()
            .join("someones-backup");
        std::fs::create_dir_all(stray.join("mods")).expect("create stray dir");

        let report = reconcile(&registry).expect("reconcile");
        assert!(report.reindexed.is_empty());
        assert!(registry.list_clients().expect("list").is_empty());
        assert!(stray.is_dir(), "a stray directory must not be touched");
    }

    #[test]
    fn unreadable_metadata_is_reported_rather_than_repaired_or_deleted() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let root = registry.paths().persistent_instances_dir().join("broken");
        std::fs::create_dir_all(metadata::metadata_dir(&root)).expect("create dirs");
        std::fs::write(metadata::client_metadata_path(&root), "{ truncated").expect("write");

        let report = reconcile(&registry).expect("reconcile must not fail");
        assert!(report.reindexed.is_empty());
        assert!(
            metadata::client_metadata_path(&root).is_file(),
            "the launcher must not delete what it cannot read"
        );
    }

    #[test]
    fn a_persistent_root_that_comes_back_clears_its_warning() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let record = registry
            .create_published_client(
                NewClient::new("remounted", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");

        let backup = environment.base().join("backup");
        std::fs::rename(&record.root, &backup).expect("unmount");
        reconcile(&registry).expect("reconcile");
        assert!(
            registry
                .client(record.id)
                .expect("kept")
                .attention
                .contains(&AttentionReason::MissingRoot)
        );

        std::fs::rename(&backup, &record.root).expect("remount");
        let report = reconcile(&registry).expect("reconcile");
        assert_eq!(report.recovered.len(), 1);
        assert!(!registry.client(record.id).expect("kept").needs_attention());
    }

    #[test]
    fn a_vanished_temporary_server_is_dropped_like_a_temporary_instance() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new(
                "temp server",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");

        std::fs::remove_dir_all(&server.root).expect("clear temp");

        let report = reconcile(&registry).expect("reconcile");
        assert_eq!(
            report.removed_stale_temporary[0].reference,
            RecordRef::Server(server.id)
        );
        assert!(registry.list_servers().expect("list").is_empty());
    }

    #[test]
    fn canonical_server_metadata_replaces_a_disagreeing_index_copy() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(
                NewServer::new("canonical", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        registry
            .index()
            .connection()
            .execute(
                "UPDATE servers SET name = 'stale index value' WHERE id = ?1",
                rusqlite::params![server.id.to_string()],
            )
            .expect("damage index copy");

        let report = reconcile(&registry).expect("reconcile");
        assert_eq!(report.index_repaired.len(), 1);
        assert_eq!(registry.server(server.id).expect("read").name, "canonical");
    }

    #[test]
    fn a_moved_server_root_corrects_metadata_and_the_index() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(
                NewServer::new("moved", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        let moved = registry.paths().persistent_servers_dir().join("moved-root");
        std::fs::rename(&server.root, &moved).expect("move");

        reconcile(&registry).expect("reconcile");
        let indexed = registry.server(server.id).expect("read");
        assert_eq!(indexed.root, moved);
        assert_eq!(
            metadata::read_server_metadata(&indexed.root)
                .expect("metadata")
                .root,
            indexed.root
        );
    }

    #[test]
    fn duplicate_server_roots_are_reported_without_selecting_one() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(
                NewServer::new("original", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        let duplicate = registry.paths().persistent_servers_dir().join("duplicate");
        std::fs::create_dir_all(metadata::metadata_dir(&duplicate)).expect("create");
        std::fs::copy(
            metadata::server_metadata_path(&server.root),
            metadata::server_metadata_path(&duplicate),
        )
        .expect("copy metadata");

        let report = reconcile(&registry).expect("reconcile");
        assert_eq!(report.server_conflicts.len(), 1);
        assert_eq!(report.server_conflicts[0].id, server.id);
        assert_eq!(
            registry.server(server.id).expect("unchanged").root,
            server.root
        );
    }

    fn copy_client_metadata_to(client: &ClientRecord, destination: &Path) {
        std::fs::create_dir_all(metadata::metadata_dir(destination)).expect("create");
        std::fs::copy(
            metadata::client_metadata_path(&client.root),
            metadata::client_metadata_path(destination),
        )
        .expect("copy metadata");
    }

    #[test]
    fn duplicate_unindexed_client_roots_are_reported_without_selecting_one() {
        let environment = TestEnvironment::new();
        let client = {
            let registry = environment.registry();
            registry
                .create_published_client(
                    NewClient::new("original", "1.20.1", LoaderSelection::vanilla())
                        .with_storage(StorageClass::Persistent),
                )
                .expect("create")
        };
        environment.destroy_database();
        let registry = environment.registry();
        let duplicate = registry
            .paths()
            .persistent_instances_dir()
            .join("duplicate");
        copy_client_metadata_to(&client, &duplicate);

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.client_conflicts.len(), 1);
        assert_eq!(report.client_conflicts[0].id, client.id);
        let mut expected = vec![client.root.clone(), duplicate];
        expected.sort();
        assert_eq!(report.client_conflicts[0].roots, expected);
        assert!(report.reindexed.is_empty());
        assert!(report.needs_attention());
        assert!(registry.list_clients().expect("list").is_empty());
    }

    #[test]
    fn a_copy_beside_an_indexed_client_with_its_files_in_place_is_reported() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(
                NewClient::new("original", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        let duplicate = registry
            .paths()
            .persistent_instances_dir()
            .join("duplicate");
        copy_client_metadata_to(&client, &duplicate);

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.client_conflicts.len(), 1);
        assert_eq!(report.client_conflicts[0].id, client.id);
        assert_eq!(
            registry.client(client.id).expect("unchanged").root,
            client.root
        );
    }

    #[test]
    fn a_client_a_move_in_another_process_holds_is_not_reported_as_a_conflict() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new(
                "original",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        let duplicate = registry
            .paths()
            .persistent_instances_dir()
            .join("duplicate");
        copy_client_metadata_to(&client, &duplicate);

        let other_process = hold_as_another_process(&registry, RecordRef::Client(client.id));
        let running = reconcile(&registry).expect("reconcile");
        assert!(running.client_conflicts.is_empty());

        drop(other_process);
        let settled = reconcile(&registry).expect("reconcile");
        assert_eq!(settled.client_conflicts.len(), 1);
    }

    #[test]
    fn client_identity_conflicts_do_not_stop_orphaned_servers_from_being_reindexed() {
        let environment = TestEnvironment::new();
        let (client, server) = {
            let registry = environment.registry();
            let client = registry
                .create_published_client(
                    NewClient::new("original", "1.20.1", LoaderSelection::vanilla())
                        .with_storage(StorageClass::Persistent),
                )
                .expect("create client");
            let server = registry
                .create_server(
                    NewServer::new("orphan", "1.20.1", LoaderSelection::vanilla())
                        .with_storage(StorageClass::Persistent),
                )
                .expect("create server");
            (client, server)
        };
        environment.destroy_database();
        let registry = environment.registry();
        let duplicate = registry
            .paths()
            .persistent_instances_dir()
            .join("duplicate");
        copy_client_metadata_to(&client, &duplicate);

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.client_conflicts.len(), 1);
        assert_eq!(report.reindexed.len(), 1);
        assert_eq!(report.reindexed[0].reference, RecordRef::Server(server.id));
        assert!(registry.server(server.id).is_ok());
    }

    #[test]
    fn a_complete_persistent_client_next_to_unfinished_directories_is_still_indexed() {
        let environment = TestEnvironment::new();
        let client = {
            let registry = environment.registry();
            registry
                .create_published_client(
                    NewClient::new("original", "1.20.1", LoaderSelection::vanilla())
                        .with_storage(StorageClass::Persistent),
                )
                .expect("create")
        };
        environment.destroy_database();
        let registry = environment.registry();
        let unfinished = registry
            .paths()
            .persistent_instances_dir()
            .join("unfinished");
        copy_client_metadata_to(&client, &unfinished);
        std::fs::write(metadata::incomplete_marker_path(&unfinished), "").expect("marker");

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.client_conflicts.is_empty());
        assert_eq!(report.unfinished_instances.len(), 1);
        assert_eq!(
            registry.client(client.id).expect("indexed").root,
            client.root
        );
    }

    #[test]
    fn corrupt_server_metadata_keeps_the_sqlite_recovery_copy_unhealthy() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(
                NewServer::new("recoverable", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        std::fs::write(metadata::server_metadata_path(&server.root), "{broken")
            .expect("damage metadata");

        let report = reconcile(&registry).expect("reconcile");
        assert_eq!(report.persistence_unhealthy.len(), 1);
        assert_eq!(registry.server(server.id).expect("recovery copy"), server);
    }

    fn fail_index_writes(registry: &Registry, failing: bool) {
        let pragma = if failing {
            "PRAGMA query_only = ON"
        } else {
            "PRAGMA query_only = OFF"
        };
        registry
            .index()
            .connection()
            .execute_batch(pragma)
            .expect("toggle index writes");
    }

    #[test]
    fn canonical_client_metadata_replaces_a_disagreeing_index_copy() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let mut client = registry
            .create_published_client(
                NewClient::new("before", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");

        // The metadata write lands and the index write fails.
        client.name = "after".to_owned();
        fail_index_writes(&registry, true);
        registry
            .update_client(&mut client)
            .expect_err("the index write fails");
        fail_index_writes(&registry, false);
        assert_eq!(registry.client(client.id).expect("read").name, "before");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.index_repaired.len(), 1);
        assert_eq!(
            report.index_repaired[0].reference,
            RecordRef::Client(client.id)
        );
        let repaired = registry.client(client.id).expect("read");
        assert_eq!(repaired.name, "after");
        assert_eq!(
            repaired.updated_at,
            metadata::read_client_metadata(&client.root)
                .expect("metadata")
                .updated_at,
            "mirroring must not restamp the record it mirrors"
        );
        assert!(
            reconcile(&registry).expect("second").is_empty(),
            "an agreeing index needs nothing"
        );
    }

    #[test]
    fn canonical_client_metadata_repairs_launch_history_and_settings() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let mut client = registry
            .create_published_client(
                NewClient::new("history", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");

        client.last_launched_at = Some(Utc::now());
        client.allow_pack_mod_version_changes = !client.allow_pack_mod_version_changes;
        client.add_attention(AttentionReason::InterruptedMutation);
        fail_index_writes(&registry, true);
        registry
            .update_client(&mut client)
            .expect_err("the index write fails");
        fail_index_writes(&registry, false);

        reconcile(&registry).expect("reconcile");

        assert_eq!(registry.client(client.id).expect("read"), client);
    }

    #[test]
    fn a_persistent_root_that_comes_back_still_reports_recovery_after_an_index_only_warning() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let record = registry
            .create_published_client(
                NewClient::new("remounted", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        let backup = environment.base().join("backup");
        std::fs::rename(&record.root, &backup).expect("unmount");
        reconcile(&registry).expect("reconcile");
        std::fs::rename(&backup, &record.root).expect("remount");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.recovered.len(), 1);
        assert!(
            report.index_repaired.is_empty(),
            "the warning lives in the index alone and is not a disagreement"
        );
    }

    #[test]
    fn a_client_edited_while_its_root_was_missing_keeps_the_edit_on_remount() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let record = registry
            .create_published_client(
                NewClient::new("original", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        let backup = environment.base().join("backup");
        std::fs::rename(&record.root, &backup).expect("unmount");
        reconcile(&registry).expect("reconcile");
        let mut edited = registry.client(record.id).expect("kept");
        edited.name = "renamed while unmounted".to_owned();
        registry
            .update_client(&mut edited)
            .expect("index-only edit");
        std::fs::rename(&backup, &record.root).expect("remount");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.recovered.len(), 1);
        assert!(report.index_repaired.is_empty());
        let after = registry.client(record.id).expect("read");
        assert_eq!(after.name, "renamed while unmounted");
        assert_eq!(
            metadata::read_client_metadata(&record.root)
                .expect("metadata")
                .name,
            "renamed while unmounted"
        );
    }

    /// An interrupted restore leaves the snapshot's copy where canonical
    /// metadata belongs, so it must not be mirrored into the index.
    #[test]
    fn a_client_with_an_unrecovered_restore_does_not_adopt_snapshot_metadata() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(
                NewClient::new("promoted", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        let mut snapshot_copy = client.clone();
        snapshot_copy.storage = StorageClass::Temporary;
        snapshot_copy.root = registry
            .paths()
            .instance_root(StorageClass::Temporary, client.id);
        metadata::write_client_metadata(&client.root, &snapshot_copy).expect("snapshot copy");
        metadata::mark_incomplete(&client.root).expect("marker");
        let journal = metadata::staging_dir(&client.root).join("restore-x/restore.json");
        std::fs::create_dir_all(journal.parent().expect("parent")).expect("staging");
        std::fs::write(&journal, "not json").expect("journal");

        let report = reconcile(&registry).expect("first");
        reconcile(&registry).expect("second");

        assert!(report.unfinished_instances.is_empty());
        let kept = registry.client(client.id).expect("the record survives");
        assert_eq!(kept.storage, StorageClass::Persistent);
        assert_eq!(kept.root, client.root);
    }

    #[test]
    fn canonical_client_metadata_contradicting_its_location_is_not_adopted() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(
                NewClient::new("contradiction", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        let mut damaged = client.clone();
        damaged.storage = StorageClass::Temporary;
        metadata::write_client_metadata(&client.root, &damaged).expect("damage");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.persistence_unhealthy.len(), 1);
        assert_eq!(registry.client(client.id).expect("kept"), client);
        reconcile(&registry).expect("second");
        assert!(registry.client(client.id).is_ok());
    }

    #[test]
    fn corrupt_client_metadata_keeps_the_sqlite_recovery_copy_unhealthy() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(
                NewClient::new("recoverable", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        std::fs::write(metadata::client_metadata_path(&client.root), "{broken")
            .expect("damage metadata");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.persistence_unhealthy.len(), 1);
        assert_eq!(registry.client(client.id).expect("recovery copy"), client);
    }

    #[test]
    fn a_relocated_client_gets_the_interrupted_state_check_in_the_same_pass() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new(
                "moved",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        let moved = registry
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        std::fs::create_dir_all(moved.parent().expect("parent")).expect("mkdir");
        std::fs::rename(&client.root, &moved).expect("move the files");
        let retired_file = metadata::retired_dir(&moved).join("mods/example.jar");
        std::fs::create_dir_all(retired_file.parent().expect("parent")).expect("create retired");
        std::fs::write(&retired_file, b"original mod").expect("write retired file");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.relocated.len(), 1);
        assert_eq!(report.interrupted_hosts.len(), 1);
        assert_eq!(
            report.interrupted_hosts[0].reference,
            RecordRef::Client(client.id)
        );
        assert!(
            registry
                .client(client.id)
                .expect("client")
                .attention
                .contains(&AttentionReason::InterruptedMutation)
        );
    }

    #[test]
    fn a_relocated_server_gets_the_interrupted_state_check_in_the_same_pass() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(NewServer::new(
                "moved server",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        let moved = registry
            .paths()
            .server_root(StorageClass::Persistent, server.id);
        std::fs::create_dir_all(moved.parent().expect("parent")).expect("mkdir");
        std::fs::rename(&server.root, &moved).expect("move the files");
        let staged_file = metadata::staging_dir(&moved).join("pending.jar");
        std::fs::create_dir_all(staged_file.parent().expect("parent")).expect("create staging");
        std::fs::write(&staged_file, b"pending mod").expect("write staged file");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.relocated.len(), 1);
        assert_eq!(report.interrupted_hosts.len(), 1);
        assert!(
            registry
                .server(server.id)
                .expect("server")
                .attention
                .contains(&AttentionReason::InterruptedMutation)
        );
    }

    /// A move that finishes between the listing and the claim has already
    /// updated the record the listing holds.
    #[test]
    fn a_client_moved_after_the_listing_is_judged_by_its_current_record() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(
                NewClient::new("moving", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        let stale_listing = registry.list_clients().expect("list");

        let moved = registry
            .paths()
            .persistent_instances_dir()
            .join("moved-root");
        std::fs::rename(&client.root, &moved).expect("move");
        let mut current = client.clone();
        current.root.clone_from(&moved);
        registry
            .update_client(&mut current)
            .expect("record the move");

        let mut report = ReconciliationReport::default();
        reconcile_listed_clients(&registry, &stale_listing, &mut report).expect("reconcile");

        assert!(report.is_empty());
        let after = registry.client(client.id).expect("read");
        assert_eq!(after.root, moved);
        assert!(after.attention.is_empty());
    }

    #[test]
    fn a_server_moved_after_the_listing_is_judged_by_its_current_record() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = registry
            .create_server(
                NewServer::new("moving", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create");
        let stale_listing = registry.list_servers().expect("list");

        let moved = registry.paths().persistent_servers_dir().join("moved-root");
        std::fs::rename(&server.root, &moved).expect("move");
        let mut current = server.clone();
        current.root.clone_from(&moved);
        registry
            .update_server(&mut current)
            .expect("record the move");

        let mut report = ReconciliationReport::default();
        reconcile_listed_servers(&registry, &stale_listing, &mut report).expect("reconcile");

        assert!(report.is_empty());
        assert_eq!(registry.server(server.id).expect("read").root, moved);
    }

    #[test]
    fn a_record_deleted_after_the_listing_is_skipped() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("gone", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let stale_listing = registry.list_clients().expect("list");
        registry
            .delete_client(client.id, FileDisposition::Keep)
            .expect("delete");

        let mut report = ReconciliationReport::default();
        reconcile_listed_clients(&registry, &stale_listing, &mut report).expect("reconcile");

        assert!(report.is_empty());
    }

    /// Takes the host's guard the way another process would: through its own
    /// open file, with no in-process claim, so only the kernel lock shows it.
    fn hold_as_another_process(registry: &Registry, host: RecordRef) -> std::fs::File {
        use rustix::fs::{FlockOperation, flock};

        let directory = registry.paths().move_guards_dir();
        std::fs::create_dir_all(&directory).unwrap();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join(crate::registry::host_guard_name(host)))
            .unwrap();
        flock(&file, FlockOperation::NonBlockingLockExclusive).unwrap();
        file
    }

    #[test]
    fn a_restore_another_process_holds_is_not_rolled_back() {
        use crate::restore::{RestorePart, RestorePartState, prepare_restore};

        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let game = metadata::client_game_dir(&client.root);
        std::fs::write(game.join("state"), b"original").unwrap();
        let _other_process = hold_as_another_process(&registry, RecordRef::Client(client.id));
        let transaction = prepare_restore(
            &client.root,
            ephervex_model::SnapshotId::new(),
            vec![RestorePartState {
                part: RestorePart::ClientGame,
                original_present: true,
                replacement_present: true,
            }],
        )
        .unwrap();
        let incoming = transaction.incoming_path(RestorePart::ClientGame);
        std::fs::create_dir_all(&incoming).unwrap();
        std::fs::write(incoming.join("state"), b"restored").unwrap();
        transaction.publish().unwrap();

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.restores_rolled_back.is_empty());
        assert!(report.interrupted_hosts.is_empty());
        assert!(report.unfinished_instances.is_empty());
        assert_eq!(std::fs::read(game.join("state")).unwrap(), b"restored");
        transaction.finish().expect("the restore still finishes");
    }

    /// Publishes a restore of the server's canonical metadata that replaces it
    /// with `replacement` and is never finished, as a crash would leave it.
    fn leave_server_restore_unfinished(
        environment: &TestEnvironment,
        server: &ServerRecord,
        replacement: &ServerRecord,
    ) -> crate::restore::RestoreTransaction {
        use crate::restore::{RestorePart, RestorePartState, prepare_restore};

        let transaction = prepare_restore(
            &server.root,
            ephervex_model::SnapshotId::new(),
            vec![RestorePartState {
                part: RestorePart::ServerMetadata,
                original_present: true,
                replacement_present: true,
            }],
        )
        .unwrap();
        let scratch = environment.base().join("snapshot-copy");
        metadata::write_server_metadata(&scratch, replacement).unwrap();
        let incoming = transaction.incoming_path(RestorePart::ServerMetadata);
        std::fs::create_dir_all(incoming.parent().unwrap()).unwrap();
        std::fs::copy(metadata::server_metadata_path(&scratch), &incoming).unwrap();
        transaction.publish().unwrap();
        transaction
    }

    fn snapshot_copy_of_a_temporary_server(
        registry: &Registry,
        server: &ServerRecord,
    ) -> ServerRecord {
        let mut copy = server.clone();
        copy.name = "snapshot copy".to_owned();
        copy.storage = StorageClass::Temporary;
        copy.root = registry
            .paths()
            .server_root(StorageClass::Temporary, server.id);
        copy
    }

    fn make_restore_unrecoverable(transaction: &crate::restore::RestoreTransaction) {
        std::fs::write(transaction.directory().join("restore.json"), "not json").unwrap();
    }

    fn persistent_server(registry: &Registry, name: &str) -> ServerRecord {
        registry
            .create_server(
                NewServer::new(name, "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create")
    }

    #[test]
    fn an_unindexed_server_with_an_interrupted_restore_is_rolled_back_before_it_is_indexed() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = persistent_server(&registry, "original");
        let snapshot = snapshot_copy_of_a_temporary_server(&registry, &server);
        let _transaction = leave_server_restore_unfinished(&environment, &server, &snapshot);
        registry
            .delete_server(server.id, FileDisposition::Keep)
            .expect("lose the index row");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.restores_rolled_back.len(), 1);
        assert_eq!(report.reindexed.len(), 1);
        assert!(!metadata::is_incomplete(&server.root));
        assert_eq!(
            registry.server(server.id).expect("indexed").name,
            "original"
        );
        assert_eq!(
            metadata::read_server_metadata(&server.root)
                .expect("metadata")
                .name,
            "original"
        );
    }

    #[test]
    fn an_unindexed_server_whose_restore_cannot_be_recovered_is_reported_and_left_unindexed() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = persistent_server(&registry, "original");
        let snapshot = snapshot_copy_of_a_temporary_server(&registry, &server);
        let transaction = leave_server_restore_unfinished(&environment, &server, &snapshot);
        make_restore_unrecoverable(&transaction);
        registry
            .delete_server(server.id, FileDisposition::Keep)
            .expect("lose the index row");

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.reindexed.is_empty());
        assert_eq!(report.unfinished_instances.len(), 1);
        assert_eq!(
            report.unfinished_instances[0].reference,
            RecordRef::Server(server.id)
        );
        assert!(registry.server(server.id).is_err());
        assert!(metadata::is_incomplete(&server.root));
        assert_eq!(
            metadata::read_server_metadata(&server.root)
                .expect("metadata")
                .name,
            "snapshot copy",
            "the root is left exactly as it was found"
        );
    }

    #[test]
    fn an_unindexed_server_without_restore_state_is_indexed_when_its_claim_cannot_be_tried() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = persistent_server(&registry, "plain");
        registry
            .delete_server(server.id, FileDisposition::Keep)
            .expect("lose the index row");
        let guards = registry.paths().move_guards_dir();
        std::fs::remove_dir_all(&guards).ok();
        std::fs::write(&guards, b"not a directory").expect("block the guard directory");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.reindexed.len(), 1);
        assert!(report.unfinished_instances.is_empty());
        assert_eq!(registry.server(server.id).expect("indexed").name, "plain");
    }

    #[test]
    fn an_unindexed_server_a_restore_in_another_process_holds_is_skipped() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = persistent_server(&registry, "original");
        let snapshot = snapshot_copy_of_a_temporary_server(&registry, &server);
        let other_process = hold_as_another_process(&registry, RecordRef::Server(server.id));
        let transaction = leave_server_restore_unfinished(&environment, &server, &snapshot);
        registry
            .delete_server(server.id, FileDisposition::Keep)
            .expect("lose the index row");

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.is_empty());
        assert!(registry.server(server.id).is_err());
        assert!(metadata::is_incomplete(&server.root));

        drop(other_process);
        transaction.finish().expect("the restore still finishes");
    }

    #[test]
    fn an_indexed_server_with_an_unrecovered_restore_does_not_adopt_snapshot_metadata() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = persistent_server(&registry, "promoted");
        let snapshot = snapshot_copy_of_a_temporary_server(&registry, &server);
        let transaction = leave_server_restore_unfinished(&environment, &server, &snapshot);
        make_restore_unrecoverable(&transaction);

        let report = reconcile(&registry).expect("first");
        reconcile(&registry).expect("second");

        assert_eq!(report.persistence_unhealthy.len(), 1);
        assert!(
            report.unfinished_instances.is_empty(),
            "an indexed server is not an unindexed directory"
        );
        let kept = registry.server(server.id).expect("the record survives");
        assert_eq!(kept.name, "promoted");
        assert_eq!(kept.storage, StorageClass::Persistent);
        assert_eq!(kept.root, server.root);
    }

    #[test]
    fn an_indexed_server_a_restore_in_another_process_holds_keeps_its_index_row() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = persistent_server(&registry, "promoted");
        let snapshot = snapshot_copy_of_a_temporary_server(&registry, &server);
        let other_process = hold_as_another_process(&registry, RecordRef::Server(server.id));
        let transaction = leave_server_restore_unfinished(&environment, &server, &snapshot);

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.restores_rolled_back.is_empty());
        assert_eq!(registry.server(server.id).expect("kept"), server);

        drop(other_process);
        transaction.finish().expect("the restore still finishes");
    }

    /// Publishes a restore of the client's canonical metadata that replaces it
    /// with `replacement` and is never finished, as a crash would leave it.
    fn leave_client_restore_unfinished(
        environment: &TestEnvironment,
        client: &ClientRecord,
        replacement: &ClientRecord,
    ) -> crate::restore::RestoreTransaction {
        use crate::restore::{RestorePart, RestorePartState, prepare_restore};

        let transaction = prepare_restore(
            &client.root,
            ephervex_model::SnapshotId::new(),
            vec![RestorePartState {
                part: RestorePart::ClientMetadata,
                original_present: true,
                replacement_present: true,
            }],
        )
        .unwrap();
        let scratch = environment.base().join("snapshot-copy");
        metadata::write_client_metadata(&scratch, replacement).unwrap();
        let incoming = transaction.incoming_path(RestorePart::ClientMetadata);
        std::fs::create_dir_all(incoming.parent().unwrap()).unwrap();
        std::fs::copy(metadata::client_metadata_path(&scratch), &incoming).unwrap();
        transaction.publish().unwrap();
        transaction
    }

    fn snapshot_copy_of_a_persistent_client(
        registry: &Registry,
        client: &ClientRecord,
    ) -> ClientRecord {
        let mut copy = client.clone();
        copy.name = "snapshot copy".to_owned();
        copy.storage = StorageClass::Temporary;
        copy.root = registry
            .paths()
            .instance_root(StorageClass::Temporary, client.id);
        copy
    }

    fn persistent_client(registry: &Registry, name: &str) -> ClientRecord {
        registry
            .create_published_client(
                NewClient::new(name, "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create")
    }

    #[test]
    fn an_unindexed_client_with_an_interrupted_restore_is_rolled_back_before_it_is_indexed() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = persistent_client(&registry, "original");
        let snapshot = snapshot_copy_of_a_persistent_client(&registry, &client);
        let _transaction = leave_client_restore_unfinished(&environment, &client, &snapshot);
        registry
            .delete_client(client.id, FileDisposition::Keep)
            .expect("lose the index row");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.restores_rolled_back.len(), 1);
        assert_eq!(report.reindexed.len(), 1);
        assert!(report.unfinished_instances.is_empty());
        assert!(!metadata::is_incomplete(&client.root));
        assert_eq!(
            registry.client(client.id).expect("indexed").name,
            "original"
        );
        assert_eq!(
            metadata::read_client_metadata(&client.root)
                .expect("metadata")
                .name,
            "original"
        );
    }

    #[test]
    fn an_unindexed_client_whose_restore_cannot_be_recovered_is_reported_and_left_unindexed() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = persistent_client(&registry, "original");
        let snapshot = snapshot_copy_of_a_persistent_client(&registry, &client);
        let transaction = leave_client_restore_unfinished(&environment, &client, &snapshot);
        make_restore_unrecoverable(&transaction);
        registry
            .delete_client(client.id, FileDisposition::Keep)
            .expect("lose the index row");

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.reindexed.is_empty());
        assert_eq!(report.unfinished_instances.len(), 1);
        assert_eq!(
            report.unfinished_instances[0].reference,
            RecordRef::Client(client.id)
        );
        assert!(registry.client(client.id).is_err());
        assert!(metadata::is_incomplete(&client.root));
        assert_eq!(
            metadata::read_client_metadata(&client.root)
                .expect("metadata")
                .name,
            "snapshot copy",
            "the root is left exactly as it was found"
        );
    }

    #[test]
    fn an_unindexed_client_a_restore_in_another_process_holds_is_skipped() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = persistent_client(&registry, "original");
        let snapshot = snapshot_copy_of_a_persistent_client(&registry, &client);
        let other_process = hold_as_another_process(&registry, RecordRef::Client(client.id));
        let transaction = leave_client_restore_unfinished(&environment, &client, &snapshot);
        registry
            .delete_client(client.id, FileDisposition::Keep)
            .expect("lose the index row");

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.is_empty());
        assert!(registry.client(client.id).is_err());
        assert!(metadata::is_incomplete(&client.root));

        drop(other_process);
        transaction.finish().expect("the restore still finishes");
    }

    #[test]
    fn an_unindexed_client_never_finished_is_reported_when_its_claim_is_free() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = persistent_client(&registry, "half built");
        metadata::mark_incomplete(&client.root).expect("marker");
        registry
            .delete_client(client.id, FileDisposition::Keep)
            .expect("lose the index row");

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.reindexed.is_empty());
        assert!(report.restores_rolled_back.is_empty());
        assert_eq!(report.unfinished_instances.len(), 1);
        assert!(registry.client(client.id).is_err());
    }

    #[test]
    fn an_indexed_client_with_an_unrecovered_restore_is_not_an_unfinished_directory() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = persistent_client(&registry, "promoted");
        let snapshot = snapshot_copy_of_a_persistent_client(&registry, &client);
        let transaction = leave_client_restore_unfinished(&environment, &client, &snapshot);
        make_restore_unrecoverable(&transaction);

        let report = reconcile(&registry).expect("reconcile");

        assert!(
            report.unfinished_instances.is_empty(),
            "an indexed client is not an unindexed directory"
        );
        let kept = registry.client(client.id).expect("kept");
        assert_eq!(kept.name, "promoted");
        assert_eq!(kept.root, client.root);
    }

    #[test]
    fn an_interrupted_duplicate_of_an_indexed_client_is_still_reported() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = persistent_client(&registry, "original");
        let unfinished = registry
            .paths()
            .persistent_instances_dir()
            .join("unfinished");
        copy_client_metadata_to(&client, &unfinished);
        metadata::mark_incomplete(&unfinished).expect("marker");

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.client_conflicts.is_empty());
        assert_eq!(report.unfinished_instances.len(), 1);
        assert_eq!(report.unfinished_instances[0].root, unfinished);
        assert_eq!(registry.client(client.id).expect("kept"), client);
    }

    #[test]
    fn canonical_server_metadata_contradicting_its_location_is_not_adopted() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = persistent_server(&registry, "contradiction");
        let mut damaged = server.clone();
        damaged.storage = StorageClass::Temporary;
        metadata::write_server_metadata(&server.root, &damaged).expect("damage");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.persistence_unhealthy.len(), 1);
        assert_eq!(registry.server(server.id).expect("kept"), server);
        reconcile(&registry).expect("second");
        assert!(registry.server(server.id).is_ok());
    }

    #[test]
    fn a_server_edited_while_its_root_was_missing_keeps_the_edit_on_remount() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = persistent_server(&registry, "original");
        let backup = environment.base().join("backup");
        std::fs::rename(&server.root, &backup).expect("unmount");
        reconcile(&registry).expect("reconcile");
        let mut edited = registry.server(server.id).expect("kept");
        edited.name = "renamed while unmounted".to_owned();
        registry
            .update_server(&mut edited)
            .expect("index-only edit");
        std::fs::rename(&backup, &server.root).expect("remount");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.recovered.len(), 1);
        assert!(report.index_repaired.is_empty());
        let after = registry.server(server.id).expect("read");
        assert_eq!(after.name, "renamed while unmounted");
        assert!(!after.attention.contains(&AttentionReason::MissingRoot));
        assert_eq!(
            metadata::read_server_metadata(&server.root)
                .expect("metadata")
                .name,
            "renamed while unmounted"
        );
    }

    #[test]
    fn a_server_whose_root_comes_back_unedited_is_recovered_without_a_repair() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let server = persistent_server(&registry, "remounted");
        let backup = environment.base().join("backup");
        std::fs::rename(&server.root, &backup).expect("unmount");
        reconcile(&registry).expect("reconcile");
        std::fs::rename(&backup, &server.root).expect("remount");

        let report = reconcile(&registry).expect("reconcile");

        assert_eq!(report.recovered.len(), 1);
        assert!(
            report.index_repaired.is_empty(),
            "the warning lives in the index alone and is not a disagreement"
        );
        assert!(
            !registry
                .server(server.id)
                .expect("read")
                .attention
                .contains(&AttentionReason::MissingRoot)
        );
    }

    #[test]
    fn a_move_another_process_holds_leaves_its_staging_directory_unreported() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let id = ServerId::new();
        let staging = registry
            .paths()
            .persistent_servers_dir()
            .join(format!(".{id}.moving"));
        std::fs::create_dir_all(&staging).unwrap();

        let other_process = hold_as_another_process(&registry, RecordRef::Server(id));
        let running = reconcile(&registry).expect("reconcile");
        assert!(running.unfinished_instances.is_empty());

        drop(other_process);
        let abandoned = reconcile(&registry).expect("reconcile");
        assert_eq!(abandoned.unfinished_instances.len(), 1);
        assert!(staging.is_dir());
    }
}
