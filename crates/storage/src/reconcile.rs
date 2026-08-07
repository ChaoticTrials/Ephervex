//! Startup reconciliation between the index and the filesystem
//! (ADR 0006).
//!
//! Three rules, and the whole module exists to keep them apart:
//!
//! 1. **A vanished temporary root is expected.** It means the operating system
//!    cleared its temporary directory, which is precisely the lifecycle the
//!    product delegates to it. The index entry is dropped silently — no
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
}

impl ReconciliationReport {
    /// Whether the index already agreed with the filesystem.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.removed_stale_temporary.is_empty()
            && self.missing_persistent.is_empty()
            && self.reindexed.is_empty()
            && self.recovered.is_empty()
    }

    /// Whether anything here needs the user's attention.
    ///
    /// A cleared temporary root does not; a missing persistent one does.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        !self.missing_persistent.is_empty()
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

        if record.root.is_dir() {
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
    let known: HashSet<ServerId> = registry
        .list_servers()?
        .into_iter()
        .map(|record| record.id)
        .collect();

    for directory in candidate_directories(&registry.paths().persistent_servers_dir()) {
        if !metadata::is_server_root(&directory) {
            continue;
        }

        let mut record = match metadata::read_server_metadata(&directory) {
            Ok(record) => record,
            Err(error) => {
                tracing::warn!(
                    path = %directory.display(),
                    %error,
                    "ignoring an unreadable server metadata file during reconciliation"
                );
                continue;
            }
        };

        if known.contains(&record.id) {
            continue;
        }

        record.root.clone_from(&directory);
        record.storage = StorageClass::Persistent;

        // A re-indexed server may reference a client that is no longer indexed.
        // Dropping the dangling link keeps the foreign key satisfiable; the
        // server itself is still worth recovering.
        if let Some(client) = record.linked_client_id {
            if registry.client(client).is_err() {
                tracing::warn!(
                    server_id = %record.id,
                    client_id = %client,
                    "re-indexed server references an unknown client; clearing the link"
                );
                record.linked_client_id = None;
            }
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
            .create_client(NewClient::new("temp", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let persistent = registry
            .create_client(
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

    #[test]
    fn only_temporary_records_are_dropped_when_both_kinds_vanish_together() {
        let environment = TestEnvironment::new();
        let registry = environment.registry();

        for _ in 0..3 {
            registry
                .create_client(NewClient::new("temp", "1.20.1", LoaderSelection::vanilla()))
                .expect("create");
        }
        registry
            .create_client(
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
            .create_client(NewClient::new("temp", "1.20.1", LoaderSelection::vanilla()))
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
                .create_client(
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
                .create_client(NewClient::new("temp", "1.20.1", LoaderSelection::vanilla()))
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
            .create_client(
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
}
