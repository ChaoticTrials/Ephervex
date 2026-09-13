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
use crate::records::FileDisposition;
use crate::registry::{Registry, RegistryError};

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
    /// Indexed recovery copies whose canonical metadata is unavailable or
    /// unusable. These remain visible but must not be launched or mutated.
    pub persistence_unhealthy: Vec<ReconciledRecord>,
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
            && self.persistence_unhealthy.is_empty()
            && self.server_conflicts.is_empty()
            && self.unfinished_instances.is_empty()
    }

    /// Whether anything here needs the user's attention.
    ///
    /// A cleared temporary root does not; a missing persistent one does.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        !self.missing_persistent.is_empty()
            || !self.persistence_unhealthy.is_empty()
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
            persistence_unhealthy = report.persistence_unhealthy.len(),
            server_conflicts = report.server_conflicts.len(),
            "startup reconciliation complete"
        );
    }

    Ok(report)
}

fn reconcile_clients(
    registry: &Registry,
    report: &mut ReconciliationReport,
) -> Result<(), RegistryError> {
    for mut record in registry.list_clients()? {
        let entry = ReconciledRecord {
            reference: RecordRef::Client(record.id),
            name: record.name.clone(),
            root: record.root.clone(),
            storage: record.storage,
        };

        if record.storage == StorageClass::Temporary
            && record.root != registry.paths().instance_root(record.storage, record.id)
        {
            // Temporary roots are launcher-owned and UID-scoped. An older or
            // tampered record pointing elsewhere must not bypass that boundary;
            // forget it without deleting the path, which may belong to a user
            // or to another account on the machine.
            registry.delete_client(record.id, FileDisposition::Keep)?;
            report.removed_stale_temporary.push(entry);
            continue;
        }

        if record.root.is_dir() {
            // The disk came back. Clearing the warning here is what stops a
            // remounted drive from leaving a permanent "needs attention" badge.
            if record.attention.contains(&AttentionReason::MissingRoot) {
                record
                    .attention
                    .retain(|reason| reason != &AttentionReason::MissingRoot);
                registry.update_client(&mut record)?;
                report.recovered.push(entry);
            }
            continue;
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
                record.root.clone_from(&moved);
                record.storage = StorageClass::Persistent;
                registry.update_client(&mut record)?;
                tracing::info!(
                    instance_id = %record.id,
                    root = %moved.display(),
                    "an instance's files were found at the persistent root; the record followed them"
                );
                report.relocated.push(ReconciledRecord {
                    reference: RecordRef::Client(record.id),
                    name: record.name.clone(),
                    root: moved,
                    storage: StorageClass::Persistent,
                });
                continue;
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
    }
    Ok(())
}

fn reconcile_servers(
    registry: &Registry,
    report: &mut ReconciliationReport,
) -> Result<(), RegistryError> {
    for mut record in registry.list_servers()? {
        let entry = ReconciledRecord {
            reference: RecordRef::Server(record.id),
            name: record.name.clone(),
            root: record.root.clone(),
            storage: record.storage,
        };

        if record.storage == StorageClass::Temporary
            && record.root != registry.paths().server_root(record.storage, record.id)
        {
            registry.delete_server(record.id, FileDisposition::Keep)?;
            report.removed_stale_temporary.push(entry);
            continue;
        }

        if record.root.is_dir() {
            match metadata::read_server_metadata(&record.root) {
                Ok(mut canonical) if canonical.id == record.id => {
                    canonical.root.clone_from(&record.root);
                    if canonical != record {
                        metadata::write_server_metadata(&record.root, &canonical)?;
                        registry.synchronize_server_index(&canonical)?;
                        report.index_repaired.push(ReconciledRecord {
                            reference: RecordRef::Server(canonical.id),
                            name: canonical.name.clone(),
                            root: canonical.root.clone(),
                            storage: canonical.storage,
                        });
                    }
                    record = canonical;
                }
                Ok(_) | Err(_) => {
                    tracing::warn!(
                        server_id = %record.id,
                        root = %record.root.display(),
                        "server canonical metadata is unhealthy; retaining only the recovery copy"
                    );
                    report.persistence_unhealthy.push(entry);
                    continue;
                }
            }
            if record.attention.contains(&AttentionReason::MissingRoot) {
                record
                    .attention
                    .retain(|reason| reason != &AttentionReason::MissingRoot);
                registry.update_server(&mut record)?;
                report.recovered.push(entry);
            }
            continue;
        }

        if record.storage.vanishing_is_expected() {
            // The same question a client is asked: were these files moved to
            // the persistent root rather than cleared away (ADR 0025)?
            let moved = registry
                .paths()
                .server_root(StorageClass::Persistent, record.id);
            if is_finished_server_root(&moved, record.id) {
                record.root.clone_from(&moved);
                record.storage = StorageClass::Persistent;
                let _committed = registry.update_server_committed(&mut record)?;
                tracing::info!(
                    server_id = %record.id,
                    root = %moved.display(),
                    "a server's files were found at the persistent root; the record followed them"
                );
                report.relocated.push(ReconciledRecord {
                    reference: RecordRef::Server(record.id),
                    name: record.name.clone(),
                    root: moved,
                    storage: StorageClass::Persistent,
                });
                continue;
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
    let known: HashSet<InstanceId> = registry
        .list_clients()?
        .into_iter()
        .map(|record| record.id)
        .collect();

    for directory in candidate_directories(&registry.paths().persistent_instances_dir()) {
        if !metadata::is_client_instance_root(&directory) {
            continue;
        }

        let mut record = match metadata::read_client_metadata(&directory) {
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
        if metadata::is_incomplete(&directory) {
            tracing::warn!(
                instance_id = %record.id,
                path = %directory.display(),
                "an instance directory was never finished; leaving it unindexed"
            );
            report.unfinished_instances.push(ReconciledRecord {
                reference: RecordRef::Client(record.id),
                name: record.name.clone(),
                root: directory.clone(),
                storage: StorageClass::Persistent,
            });
            continue;
        }

        if known.contains(&record.id) {
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

fn reindex_orphaned_servers(
    registry: &Registry,
    report: &mut ReconciliationReport,
) -> Result<(), RegistryError> {
    let indexed = registry.list_servers()?;
    let known: HashSet<ServerId> = indexed.iter().map(|record| record.id).collect();

    let mut discovered = std::collections::BTreeMap::<ServerId, Vec<PathBuf>>::new();
    for directory in candidate_directories(&registry.paths().persistent_servers_dir()) {
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
        let mut record = metadata::read_server_metadata(&directory)?;

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

/// Whether a finished client instance with this id is standing at `root`.
///
/// All three conditions matter. The directory has to look like an instance, its
/// metadata has to name this instance rather than another one, and it must not
/// carry the incomplete marker -- a move that was interrupted partway through
/// copying leaves a marked directory, and adopting that would hand the user a
/// half-copied instance while the whole one is still where it was.
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
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

    /// The destination of an interrupted *copy* is marked incomplete. Adopting
    /// it would hand the user a half-copied instance while the whole one is
    /// still sitting in the temporary root.
    #[test]
    fn a_half_copied_destination_is_not_mistaken_for_a_finished_move() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();
        let client = registry
            .create_published_client(NewClient::new(
                "repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");

        let destination = registry
            .paths()
            .instance_root(StorageClass::Persistent, client.id);
        std::fs::create_dir_all(&destination).expect("mkdir");
        crate::metadata::mark_incomplete(&destination).expect("mark");
        crate::metadata::write_client_metadata(&destination, &client).expect("metadata");
        std::fs::remove_dir_all(&client.root).expect("clear temp");

        let report = reconcile(&registry).expect("reconcile");

        assert!(report.relocated.is_empty());
        assert_eq!(
            report.removed_stale_temporary.len(),
            1,
            "an unfinished copy is not the instance"
        );
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
}
