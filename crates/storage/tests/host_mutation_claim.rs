//! A snapshot restore or storage move holds its host's claim, and
//! reconciliation never treats a host whose claim is held as abandoned.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ephervex_model::{InstanceId, LoaderSelection, SnapshotId, StorageClass};
use ephervex_storage::{
    ClientRecord, NewClient, NewServer, Paths, RecordRef, Registry, RegistryError, RestorePart,
    RestorePartState, RestoreTransaction, ServerRecord, metadata, prepare_restore, reconcile,
};
use tempfile::TempDir;

fn isolated_paths(base: &TempDir) -> Paths {
    let paths = Paths::rooted_at(base.path());
    paths.ensure_base_directories().expect("create roots");
    paths
}

fn client_with_state(registry: &Registry, state: &[u8]) -> ClientRecord {
    let client = registry
        .create_client(NewClient::new("host", "1.20.1", LoaderSelection::vanilla()))
        .expect("create");
    registry.publish_client(&client).expect("publish");
    std::fs::write(metadata::client_game_dir(&client.root).join("state"), state).expect("state");
    client
}

/// Publishes a replacement game directory and leaves the restore unfinished.
fn published_restore(client: &ClientRecord) -> RestoreTransaction {
    let transaction = prepare_restore(
        &client.root,
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
    std::fs::write(incoming.join("state"), b"restored").expect("replacement");
    transaction.publish().expect("publish");
    transaction
}

fn state_of(client: &ClientRecord) -> Vec<u8> {
    std::fs::read(metadata::client_game_dir(&client.root).join("state")).expect("state")
}

#[test]
fn reconciling_in_another_launcher_leaves_a_running_restore_alone() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");
    let client = client_with_state(&registry, b"original");

    let claim = registry
        .claim_host(RecordRef::Client(client.id))
        .expect("claim");
    let transaction = published_restore(&client);

    let other_launcher = Registry::open(paths).expect("second connection");
    let report = reconcile(&other_launcher).expect("reconcile");

    assert!(report.restores_rolled_back.is_empty());
    assert!(report.interrupted_hosts.is_empty());
    assert_eq!(state_of(&client), b"restored");
    transaction.finish().expect("the restore still finishes");
    drop(claim);
    assert_eq!(state_of(&client), b"restored");
    assert!(!metadata::is_incomplete(&client.root));
}

#[test]
fn reconciling_in_another_launcher_leaves_a_restore_that_is_still_preparing_alone() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");
    let client = client_with_state(&registry, b"original");

    let _claim = registry
        .claim_host(RecordRef::Client(client.id))
        .expect("claim");
    let transaction = prepare_restore(&client.root, SnapshotId::new(), vec![]).expect("prepare");
    std::fs::write(transaction.directory().join("staged"), b"half copied").expect("staging");

    let report = reconcile(&Registry::open(paths).expect("second connection")).expect("reconcile");

    assert!(report.interrupted_hosts.is_empty());
    assert!(transaction.directory().join("staged").is_file());
}

#[test]
fn a_restore_the_holder_abandoned_is_rolled_back_once_its_claim_is_gone() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");
    let client = client_with_state(&registry, b"original");

    let claim = registry
        .claim_host(RecordRef::Client(client.id))
        .expect("claim");
    let transaction = published_restore(&client);
    drop(claim);

    let report = reconcile(&Registry::open(paths).expect("second connection")).expect("reconcile");

    assert_eq!(report.restores_rolled_back.len(), 1);
    assert_eq!(state_of(&client), b"original");
    assert!(!transaction.directory().exists());
}

#[test]
fn a_move_is_refused_while_a_restore_holds_the_host() {
    let base = tempfile::tempdir().expect("temp dir");
    let registry = Registry::open(isolated_paths(&base)).expect("open registry");
    let client = client_with_state(&registry, b"original");
    let _restore = registry
        .claim_host(RecordRef::Client(client.id))
        .expect("claim");

    let error = registry
        .claim_host(RecordRef::Client(client.id))
        .expect_err("the restore holds the host");

    assert!(matches!(error, RegistryError::HostBusy { .. }));
    assert!(client.root.is_dir());
}

#[test]
fn a_move_needs_the_claim_of_the_host_it_moves() {
    let base = tempfile::tempdir().expect("temp dir");
    let registry = Registry::open(isolated_paths(&base)).expect("open registry");
    let client = client_with_state(&registry, b"original");
    let other = client_with_state(&registry, b"other");
    let claim_of_other = registry
        .claim_host(RecordRef::Client(other.id))
        .expect("claim");

    let error = registry
        .relocate_client(
            client.id,
            &claim_of_other,
            StorageClass::Persistent,
            &mut |_| {},
        )
        .expect_err("another host's claim does not cover this one");

    assert!(matches!(error, RegistryError::HostNotClaimed { .. }));
    assert!(client.root.is_dir());
}

#[test]
fn a_restore_is_refused_while_a_move_holds_the_host() {
    let base = tempfile::tempdir().expect("temp dir");
    let registry = Registry::open(isolated_paths(&base)).expect("open registry");
    let client = client_with_state(&registry, b"original");
    let _move = registry
        .claim_host(RecordRef::Client(client.id))
        .expect("claim");

    let other = Registry::open(isolated_paths(&base)).expect("second connection");
    let error = other
        .claim_host(RecordRef::Client(client.id))
        .expect_err("the move holds the host");

    assert!(matches!(error, RegistryError::HostBusy { .. }));
}

#[test]
fn a_running_move_s_staging_directory_is_not_reported_but_an_abandoned_one_is() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");
    let client = registry
        .create_client(
            NewClient::new("moving", "1.20.1", LoaderSelection::vanilla())
                .with_storage(StorageClass::Persistent),
        )
        .expect("create");
    registry.publish_client(&client).expect("publish");
    let staging = paths
        .persistent_instances_dir()
        .join(format!(".{}.moving", client.id));
    std::fs::create_dir_all(staging.join(".launcher")).expect("staging");
    std::fs::write(staging.join("copied.bin"), b"partial").expect("copied content");

    let claim = registry
        .claim_host(RecordRef::Client(client.id))
        .expect("claim");
    let other_launcher = Registry::open(paths.clone()).expect("second connection");
    let running = reconcile(&other_launcher).expect("reconcile");
    assert!(running.unfinished_instances.is_empty());
    assert!(staging.is_dir());

    drop(claim);
    let abandoned = reconcile(&other_launcher).expect("reconcile");
    assert_eq!(abandoned.unfinished_instances.len(), 1);
    assert_eq!(abandoned.unfinished_instances[0].root, staging);
    assert!(staging.is_dir(), "reporting never deletes");
}

#[test]
fn a_staging_directory_of_an_unindexed_instance_is_still_reported() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");
    let staging = paths
        .persistent_instances_dir()
        .join(format!(".{}.moving", InstanceId::new()));
    std::fs::create_dir_all(&staging).expect("staging");
    std::fs::write(staging.join("copied.bin"), b"partial").expect("copied content");

    let report = reconcile(&registry).expect("reconcile");

    assert_eq!(report.unfinished_instances.len(), 1);
}

fn temporary_server(registry: &Registry) -> ServerRecord {
    registry
        .create_server(NewServer::new("host", "1.20.1", LoaderSelection::vanilla()))
        .expect("create server")
}

/// Moves a client to the persistent root under its claim, as a caller must.
fn promote_client(registry: &Registry, client: &ClientRecord) -> ClientRecord {
    let claim = registry
        .claim_host(RecordRef::Client(client.id))
        .expect("claim");
    registry
        .relocate_client(client.id, &claim, StorageClass::Persistent, &mut |_| {})
        .expect("move")
        .record
}

#[test]
fn a_client_write_prepared_before_a_move_is_refused_instead_of_naming_the_vacated_root() {
    let base = tempfile::tempdir().expect("temp dir");
    let registry = Registry::open(isolated_paths(&base)).expect("open registry");
    let client = client_with_state(&registry, b"original");
    let mut stale = registry.current_client(client.id).expect("read");
    let moved = promote_client(&registry, &client);
    assert_ne!(moved.root, client.root);

    stale.name = "renamed".to_owned();
    let error = registry
        .update_client_committed(&mut stale)
        .expect_err("the host moved after the record was read");

    assert!(matches!(error, RegistryError::HostBusy { .. }), "{error}");
    let row = registry.indexed_client(client.id).expect("row");
    assert_eq!(row.root, moved.root);
    assert_eq!(row.storage, StorageClass::Persistent);
    assert_eq!(row.name, moved.name);
    assert!(moved.root.is_dir());
    assert_eq!(
        metadata::read_client_metadata(&moved.root)
            .expect("metadata")
            .name,
        moved.name
    );
    assert!(!client.root.exists(), "the refused write recreated nothing");
    assert_eq!(
        registry.client(client.id).expect("reachable").root,
        moved.root
    );
}

#[test]
fn a_client_change_made_after_a_move_lands_at_the_moved_root() {
    let base = tempfile::tempdir().expect("temp dir");
    let registry = Registry::open(isolated_paths(&base)).expect("open registry");
    let client = client_with_state(&registry, b"original");
    let moved = promote_client(&registry, &client);

    registry
        .modify_client(client.id, |record| record.name = "renamed".to_owned())
        .expect("change");

    let row = registry.indexed_client(client.id).expect("row");
    assert_eq!(row.root, moved.root);
    assert_eq!(row.name, "renamed");
    assert_eq!(
        metadata::read_client_metadata(&moved.root)
            .expect("metadata")
            .name,
        "renamed"
    );
}

#[test]
fn a_move_cannot_start_between_a_client_changes_read_and_its_write() {
    let base = tempfile::tempdir().expect("temp dir");
    let registry = Registry::open(isolated_paths(&base)).expect("open registry");
    let client = client_with_state(&registry, b"original");

    registry
        .modify_client(client.id, |record| {
            assert!(
                matches!(
                    registry.claim_host(RecordRef::Client(client.id)),
                    Err(RegistryError::HostBusy { .. })
                ),
                "the change holds the host between its read and its write"
            );
            record.name = "renamed".to_owned();
        })
        .expect("change");

    assert_eq!(
        registry.indexed_client(client.id).expect("row").name,
        "renamed"
    );
}

#[test]
fn a_client_write_is_refused_while_another_operation_holds_the_host() {
    let base = tempfile::tempdir().expect("temp dir");
    let registry = Registry::open(isolated_paths(&base)).expect("open registry");
    let client = client_with_state(&registry, b"original");
    let claim = registry
        .claim_host(RecordRef::Client(client.id))
        .expect("claim");
    let mut record = registry.current_client(client.id).expect("read");
    record.name = "renamed".to_owned();

    let refused = registry.update_client_committed(&mut record.clone());
    let changed = registry.modify_client(client.id, |current| current.name = "other".to_owned());

    assert!(matches!(refused, Err(RegistryError::HostBusy { .. })));
    assert!(matches!(changed, Err(RegistryError::HostBusy { .. })));
    registry
        .update_client_committed_claimed(&mut record, &claim)
        .expect("the holder of the claim writes");
    assert_eq!(
        registry.indexed_client(client.id).expect("row").name,
        "renamed"
    );
}

#[test]
fn a_claimed_client_write_needs_the_claim_of_its_own_host() {
    let base = tempfile::tempdir().expect("temp dir");
    let registry = Registry::open(isolated_paths(&base)).expect("open registry");
    let client = client_with_state(&registry, b"original");
    let other = client_with_state(&registry, b"other");
    let claim_of_other = registry
        .claim_host(RecordRef::Client(other.id))
        .expect("claim");
    let mut record = registry.current_client(client.id).expect("read");

    let error = registry
        .update_client_committed_claimed(&mut record, &claim_of_other)
        .expect_err("another host's claim does not cover this one");
    let changed = registry.modify_client_committed_claimed(client.id, &claim_of_other, |_| {});

    assert!(matches!(error, RegistryError::HostNotClaimed { .. }));
    assert!(matches!(changed, Err(RegistryError::HostNotClaimed { .. })));
}

#[test]
fn a_server_write_prepared_before_a_move_is_refused_instead_of_naming_the_vacated_root() {
    let base = tempfile::tempdir().expect("temp dir");
    let registry = Registry::open(isolated_paths(&base)).expect("open registry");
    let server = temporary_server(&registry);
    let mut stale = registry.current_server(server.id).expect("read");
    let claim = registry
        .claim_host(RecordRef::Server(server.id))
        .expect("claim");
    let moved = registry
        .relocate_server(server.id, &claim, StorageClass::Persistent, &mut |_| {})
        .expect("move")
        .record;
    drop(claim);
    assert_ne!(moved.root, server.root);

    stale.name = "renamed".to_owned();
    let error = registry
        .update_server_committed(&mut stale)
        .expect_err("the host moved after the record was read");

    assert!(matches!(error, RegistryError::HostBusy { .. }), "{error}");
    let row = registry.indexed_server(server.id).expect("row");
    assert_eq!(row.root, moved.root);
    assert_eq!(row.storage, StorageClass::Persistent);
    assert_eq!(row.name, moved.name);
    assert!(moved.root.is_dir());
    assert!(!server.root.exists(), "the refused write recreated nothing");
    assert_eq!(
        registry.server(server.id).expect("reachable").root,
        moved.root
    );
}

#[test]
fn a_server_write_is_refused_while_another_operation_holds_the_host() {
    let base = tempfile::tempdir().expect("temp dir");
    let registry = Registry::open(isolated_paths(&base)).expect("open registry");
    let server = temporary_server(&registry);
    let claim = registry
        .claim_host(RecordRef::Server(server.id))
        .expect("claim");
    let mut record = registry.current_server(server.id).expect("read");
    record.name = "renamed".to_owned();

    let refused = registry.update_server_committed(&mut record.clone());

    assert!(matches!(refused, Err(RegistryError::HostBusy { .. })));
    registry
        .update_server_committed_claimed(&mut record, &claim)
        .expect("the holder of the claim writes");
    assert_eq!(
        registry.indexed_server(server.id).expect("row").name,
        "renamed"
    );
}

#[test]
fn a_claimed_server_write_needs_the_claim_of_its_own_host() {
    let base = tempfile::tempdir().expect("temp dir");
    let registry = Registry::open(isolated_paths(&base)).expect("open registry");
    let server = temporary_server(&registry);
    let other = temporary_server(&registry);
    let claim_of_other = registry
        .claim_host(RecordRef::Server(other.id))
        .expect("claim");
    let mut record = registry.current_server(server.id).expect("read");

    let error = registry
        .update_server_committed_claimed(&mut record, &claim_of_other)
        .expect_err("another host's claim does not cover this one");

    assert!(matches!(error, RegistryError::HostNotClaimed { .. }));
}
