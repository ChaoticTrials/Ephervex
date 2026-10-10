//! The storage and registry support regression coverage, end to end, against the public API only.
//!
//! This test covers the complete storage and registry workflow:
//!
//! ```text
//! create temp record
//! create persistent record
//! restart registry
//! reconcile missing temp directory
//! list remaining records
//! ```
//!
//! plus "no secret is stored in config". The same sequence is driven through
//! the `ephervex` binary in `crates/cli/tests/instance_registry_cli.rs`; this file proves
//! the library behaves that way without a process boundary in the way.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ephervex_model::{AttentionReason, InstanceId, LoaderKind, LoaderSelection, StorageClass};
use ephervex_storage::{
    ClientRecord, Config, LaunchLock, LockError, NewClient, NewServer, Paths, RecordRef, Registry,
    RegistryError, SECRET_KEY_MARKERS, config, reconcile,
};
use tempfile::TempDir;

/// Makes an instance persistent under its host claim, the way a caller must.
fn promote(registry: &Registry, id: InstanceId) -> Result<ClientRecord, RegistryError> {
    let claim = registry.claim_host(RecordRef::Client(id))?;
    registry.relocate_client(id, &claim, StorageClass::Persistent, &mut |_| {})
}

/// Lays every application root, including the temporary one, inside a
/// throwaway directory. No test may touch the developer's real XDG dirs.
fn isolated_paths(base: &TempDir) -> Paths {
    let paths = Paths::rooted_at(base.path());
    paths.ensure_base_directories().expect("create roots");
    paths
}

#[test]
fn the_instance_registry_workflow_holds_end_to_end() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);

    // --- create temp record -------------------------------------------------
    // Creation and publication are two steps: an instance only becomes visible
    // to the rest of the registry once its materialization has finished.
    let temporary = {
        let registry = Registry::open(paths.clone()).expect("open registry");
        let record = registry
            .create_client(NewClient::new(
                "Temp repro",
                "1.20.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
            ))
            .expect("create temporary instance");
        registry.publish_client(&record).expect("publish");
        record
    };
    assert_eq!(
        temporary.storage,
        StorageClass::Temporary,
        "temporary is the product default "
    );
    assert!(temporary.root.starts_with(paths.temporary_instances_dir()));

    // --- create persistent record -------------------------------------------
    let persistent = {
        let registry = Registry::open(paths.clone()).expect("open registry");
        let record = registry
            .create_client(
                NewClient::new(
                    "Kept repro",
                    "1.12.2",
                    LoaderSelection::exact(LoaderKind::Forge, "14.23.5.2859"),
                )
                .with_storage(StorageClass::Persistent),
            )
            .expect("create persistent instance");
        registry.publish_client(&record).expect("publish");
        record
    };
    assert!(
        persistent
            .root
            .starts_with(paths.persistent_instances_dir())
    );

    // --- restart registry ---------------------------------------------------
    // A brand new registry over the same database file: no shared in-memory
    // state, exactly as the next launch would see it.
    let registry = Registry::open(paths.clone()).expect("reopen registry");
    assert_eq!(registry.list_clients().expect("list").len(), 2);

    // --- reconcile missing temp directory -----------------------------------
    // Simulate the operating system clearing its temporary directory, which is
    // the lifecycle ADR 0006 delegates to it.
    std::fs::remove_dir_all(paths.temporary_instances_dir()).expect("clear the temp root");

    let report = reconcile(&registry).expect("reconcile");
    assert_eq!(report.removed_stale_temporary.len(), 1);
    assert!(
        report.missing_persistent.is_empty(),
        "the persistent instance is untouched"
    );
    assert!(
        !report.needs_attention(),
        "a cleared temp root is expected, not a warning"
    );

    // --- list remaining records ---------------------------------------------
    let remaining = registry.list_clients().expect("list");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, persistent.id);
    assert_eq!(remaining[0].storage, StorageClass::Persistent);
    assert!(remaining[0].root.is_dir());
    assert!(!remaining[0].needs_attention());
}

#[test]
fn a_missing_persistent_root_is_kept_and_warned_about_rather_than_removed() {
    // The counterpart to the sequence above, and the reason
    // `StorageClass::vanishing_is_expected` exists.
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");

    let persistent = registry
        .create_client(
            NewClient::new("Kept repro", "1.20.1", LoaderSelection::vanilla())
                .with_storage(StorageClass::Persistent),
        )
        .expect("create");
    registry.publish_client(&persistent).expect("publish");

    std::fs::remove_dir_all(&persistent.root).expect("simulate an unmounted disk");

    let report = reconcile(&registry).expect("reconcile");
    assert_eq!(report.missing_persistent.len(), 1);
    assert!(report.needs_attention());

    let kept = registry.client(persistent.id).expect("the record survives");
    assert!(kept.attention.contains(&AttentionReason::MissingRoot));
}

#[test]
fn no_secret_is_stored_in_config() {
    // The other half of the storage and registry support regression coverage (ADR 0004).
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);

    let config = Config::default();
    config.save(&paths).expect("save");

    let written = std::fs::read_to_string(paths.config_file()).expect("read back");
    let lowered = written.to_ascii_lowercase();
    for marker in SECRET_KEY_MARKERS {
        assert!(
            !lowered.contains(marker),
            "the configuration file must not contain `{marker}`:\n{written}"
        );
    }

    // And the guard is enforced, not merely observed: a credential planted in
    // the open part of the schema is refused rather than written.
    let mut with_credential = Config::default();
    with_credential.unknown.insert(
        "microsoft_refresh_token".to_owned(),
        toml::Value::String("M.C123_BAY.0.U.not-a-real-token".to_owned()),
    );
    let error = with_credential
        .save(&paths)
        .expect_err("saving a credential must fail");
    assert!(matches!(error, config::ConfigError::SecretRejected { .. }));

    // The file on disk is untouched by the refused save.
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).expect("read back"),
        written
    );
}

/// The hard invariant: nothing treats an instance as complete before it is
/// published.
///
/// Enumerated on purpose rather than spot-checked: each entry point below is
/// asserted separately, so a regression names which one broke instead of
/// failing a single blanket check.
///
/// The list is maintained by hand and cannot notice an entry point added after
/// it. What makes the invariant hold is structural, and it is that every one of
/// these resolves through the index, which an unpublished instance has no row
/// in; this test is the evidence for that, not the mechanism.
#[test]
fn no_entry_point_sees_an_instance_before_it_is_published() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");

    let unpublished = registry
        .create_client(
            NewClient::new("Half built", "1.20.1", LoaderSelection::vanilla())
                .with_storage(StorageClass::Persistent),
        )
        .expect("create");

    // The directory and its canonical metadata exist -- that is what makes the
    // rest of this test worth writing.
    assert!(unpublished.root.is_dir());
    assert!(
        ephervex_storage::metadata::is_incomplete(&unpublished.root),
        "a created instance carries the marker until it is published"
    );

    // -- lookup ------------------------------------------------------------
    assert!(
        matches!(
            registry.client(unpublished.id),
            Err(ephervex_storage::RegistryError::InstanceUnknown { .. })
        ),
        "reading an unpublished instance must report it as unknown"
    );
    assert!(
        registry.list_clients().expect("list").is_empty(),
        "an unpublished instance must not be listed"
    );
    assert!(
        registry
            .linked_server_ids(unpublished.id)
            .expect("links")
            .is_empty()
    );

    // -- mutation ----------------------------------------------------------
    let mut mutated = unpublished.clone();
    assert!(
        matches!(
            registry.update_client(&mut mutated),
            Err(ephervex_storage::RegistryError::InstanceUnknown { .. })
        ),
        "an unpublished instance must not be mutable"
    );

    // -- deletion and duplication ------------------------------------------
    assert!(
        matches!(
            registry.delete_client(unpublished.id, ephervex_storage::FileDisposition::Remove),
            Err(ephervex_storage::RegistryError::InstanceUnknown { .. })
        ),
        "an unpublished instance is not deletable by id; only its creator can discard it"
    );
    assert!(
        matches!(
            registry.duplicate_client(
                unpublished.id,
                &ephervex_storage::DuplicateClient::default()
            ),
            Err(ephervex_storage::RegistryError::InstanceUnknown { .. })
        ),
        "an unpublished instance must not be duplicable"
    );
    // The refused delete left the files exactly where they were.
    assert!(unpublished.root.is_dir());

    // -- reconciliation ----------------------------------------------------
    let report = reconcile(&registry).expect("reconcile");
    assert!(
        registry.list_clients().expect("list").is_empty(),
        "reconciliation must never adopt an unfinished instance"
    );
    assert_eq!(report.unfinished_instances.len(), 1);
    assert_eq!(report.unfinished_instances[0].root, unpublished.root);
    assert!(report.needs_attention());
    assert!(
        unpublished.root.is_dir(),
        "reconciliation reports an unfinished instance; it never deletes one"
    );

    // -- publication makes it real ------------------------------------------
    registry.publish_client(&unpublished).expect("publish");
    assert!(!ephervex_storage::metadata::is_incomplete(
        &unpublished.root
    ));
    assert_eq!(
        registry.client(unpublished.id).expect("published").id,
        unpublished.id
    );
    assert_eq!(registry.list_clients().expect("list").len(), 1);
}

/// The recovery path ADR 0002 exists for must not regress: a *finished*
/// instance the database has lost is still adopted.
#[test]
fn a_finished_instance_missing_from_the_index_is_still_recovered() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");

    let record = registry
        .create_client(
            NewClient::new("Recoverable", "1.20.1", LoaderSelection::vanilla())
                .with_storage(StorageClass::Persistent),
        )
        .expect("create");
    registry.publish_client(&record).expect("publish");

    // Lose the index row, keep the directory: a database restored from an old
    // backup, or removed to force a rebuild.
    std::fs::remove_file(paths.database_file()).expect("drop the index");
    let registry = Registry::open(paths.clone()).expect("reopen registry");
    assert!(registry.list_clients().expect("list").is_empty());

    let report = reconcile(&registry).expect("reconcile");
    assert_eq!(report.reindexed.len(), 1);
    assert!(report.unfinished_instances.is_empty());
    assert_eq!(registry.list_clients().expect("list").len(), 1);
}

/// An interrupted duplication is the one unfinished instance whose metadata
/// names an id the index already knows, because the copy carries the
/// original's `instance.json` until it finishes. Testing the index first would
/// skip exactly the directory the marker was written for.
#[test]
fn an_interrupted_duplication_is_reported_even_though_its_metadata_is_already_indexed() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");

    let original = registry
        .create_client(
            NewClient::new("Original", "1.20.1", LoaderSelection::vanilla())
                .with_storage(StorageClass::Persistent),
        )
        .expect("create");
    registry.publish_client(&original).expect("publish");

    // What `duplicate_client` leaves behind when it is killed mid-copy: a
    // second root, marked incomplete, holding the original's metadata.
    let abandoned = paths
        .persistent_instances_dir()
        .join("11111111-2222-4333-8444-555555555555");
    std::fs::create_dir_all(abandoned.join(".launcher")).expect("create the abandoned root");
    std::fs::copy(
        original.root.join(".launcher/instance.json"),
        abandoned.join(".launcher/instance.json"),
    )
    .expect("copy the metadata the way the interrupted copy would have");
    std::fs::write(abandoned.join(".launcher/incomplete"), b"").expect("mark it incomplete");

    let report = reconcile(&registry).expect("reconcile");

    assert_eq!(
        report.unfinished_instances.len(),
        1,
        "an interrupted duplication must be reported, not silently kept forever"
    );
    assert_eq!(report.unfinished_instances[0].root, abandoned);
    assert!(report.needs_attention());
    // The original is untouched and still the only instance.
    assert_eq!(registry.list_clients().expect("list").len(), 1);
    assert!(abandoned.is_dir(), "reporting never deletes");
}

/// Promotion is the same instance at a new address, which is the whole thing
/// that distinguishes it from duplication (ADR 0025).
#[test]
fn a_promoted_instance_keeps_its_id_its_name_and_its_linked_servers() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");

    let original = registry
        .create_client(NewClient::new(
            "Repro",
            "1.20.1",
            LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
        ))
        .expect("create");
    registry.publish_client(&original).expect("publish");
    let server = registry
        .create_server(NewServer::linked("Repro Server", original.id))
        .expect("create server");
    std::fs::write(original.root.join("minecraft/options.txt"), "guiScale:2\n")
        .expect("a setting worth keeping");

    let promoted = promote(&registry, original.id).expect("promote");

    assert_eq!(promoted.id, original.id, "the same instance, not a copy");
    assert_eq!(promoted.name, original.name);
    assert_eq!(promoted.loader, original.loader);
    assert_eq!(promoted.storage, StorageClass::Persistent);
    assert_eq!(
        promoted.root,
        paths.instance_root(StorageClass::Persistent, original.id)
    );
    assert!(!original.root.exists(), "the instance is not in two places");
    assert_eq!(
        std::fs::read_to_string(promoted.root.join("minecraft/options.txt")).expect("read"),
        "guiScale:2\n"
    );
    assert_eq!(
        registry.linked_server_ids(original.id).expect("links"),
        vec![server.id],
        "a promoted client keeps the servers a duplicate would not have got"
    );

    // What the next launch reads, rather than what this process remembers.
    let registry = Registry::open(paths.clone()).expect("reopen registry");
    let reopened = registry.client(original.id).expect("indexed");
    assert_eq!(reopened.root, promoted.root);
    assert_eq!(reopened.storage, StorageClass::Persistent);
    assert!(reconcile(&registry).expect("reconcile").is_empty());
}

/// The destination path is derived from the instance id, so an interrupted
/// attempt stands exactly where the retry needs to land.
#[test]
fn a_promotion_retried_after_an_interrupted_copy_clears_its_own_leftover() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");

    let original = registry
        .create_client(NewClient::new(
            "Repro",
            "1.20.1",
            LoaderSelection::vanilla(),
        ))
        .expect("create");
    registry.publish_client(&original).expect("publish");

    // What a promotion killed mid-copy leaves: its hidden staging directory
    // beside the destination, never anything at the destination itself.
    let destination = paths.instance_root(StorageClass::Persistent, original.id);
    let stage = destination.with_file_name(format!(".{}.moving", original.id));
    std::fs::create_dir_all(stage.join(".launcher")).expect("create the abandoned copy");
    std::fs::copy(
        original.root.join(".launcher/instance.json"),
        stage.join(".launcher/instance.json"),
    )
    .expect("the metadata the interrupted copy carried");
    std::fs::write(stage.join("half-written.bin"), b"partial").expect("partial content");

    let promoted = promote(&registry, original.id).expect("the retry clears its own leftover");

    assert_eq!(promoted.root, destination);
    assert!(
        !stage.exists(),
        "the abandoned attempt's contents must not survive the finished move"
    );
    assert!(!destination.join("half-written.bin").exists());
    assert!(promoted.root.join("minecraft").is_dir());
}

/// Somebody else's directory at the destination is a conflict, not a leftover
/// to clear.
#[test]
fn a_promotion_refuses_a_destination_another_instance_occupies() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");

    let original = registry
        .create_client(NewClient::new(
            "Repro",
            "1.20.1",
            LoaderSelection::vanilla(),
        ))
        .expect("create");
    registry.publish_client(&original).expect("publish");

    let destination = paths.instance_root(StorageClass::Persistent, original.id);
    std::fs::create_dir_all(&destination).expect("mkdir");
    std::fs::write(destination.join("someones-file.txt"), b"not ours").expect("write");

    let error = promote(&registry, original.id).expect_err("an occupied destination is refused");

    assert!(
        matches!(error, RegistryError::RootOccupied { .. }),
        "expected an occupied-root refusal, got {error}"
    );
    assert!(
        original.root.join("minecraft").is_dir(),
        "a refused promotion leaves the instance exactly where it was"
    );
    assert!(destination.join("someones-file.txt").is_file());
}

/// Its launch lock describes a process, and the instance at the new address is
/// not running one.
#[test]
fn a_promoted_instance_does_not_carry_its_launch_lock_to_the_new_root() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");

    let original = registry
        .create_client(NewClient::new(
            "Repro",
            "1.20.1",
            LoaderSelection::vanilla(),
        ))
        .expect("create");
    registry.publish_client(&original).expect("publish");
    std::fs::write(original.root.join(".launcher/launch.lock"), b"{}").expect("a stale lock");

    let promoted = promote(&registry, original.id).expect("promote");

    assert!(!promoted.root.join(".launcher/launch.lock").exists());
}

#[test]
fn promoting_an_instance_that_is_already_persistent_changes_nothing() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = isolated_paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");

    let original = registry
        .create_client(
            NewClient::new("Kept", "1.20.1", LoaderSelection::vanilla())
                .with_storage(StorageClass::Persistent),
        )
        .expect("create");
    registry.publish_client(&original).expect("publish");

    let promoted =
        promote(&registry, original.id).expect("a no-op rather than a refusal at this level");

    assert_eq!(promoted.root, original.root);
    assert_eq!(promoted.storage, StorageClass::Persistent);
}

#[test]
fn a_move_keeps_the_launch_lock_of_a_launch_that_holds_the_instance() {
    let base = tempfile::tempdir().expect("temp dir");
    let registry = Registry::open(isolated_paths(&base)).expect("open registry");
    let original = registry
        .create_client(NewClient::new(
            "Repro",
            "1.20.1",
            LoaderSelection::vanilla(),
        ))
        .expect("create");
    registry.publish_client(&original).expect("publish");
    let launch = LaunchLock::acquire(&original.root).expect("a launch holds the instance");

    let error = promote(&registry, original.id).expect_err("a held instance is not moved");

    assert!(
        matches!(
            error,
            RegistryError::Lock(LockError::AlreadyRunning { pid, .. }) if pid == std::process::id()
        ),
        "{error}"
    );
    assert!(launch.path().is_file(), "the live lock was not removed");
    assert!(original.root.join(".launcher/instance.json").is_file());
    assert_eq!(
        registry.client(original.id).expect("record").root,
        original.root
    );
}

#[test]
fn a_move_is_refused_while_an_additional_launch_holds_the_instance() {
    let base = tempfile::tempdir().expect("temp dir");
    let registry = Registry::open(isolated_paths(&base)).expect("open registry");
    let original = registry
        .create_client(NewClient::new(
            "Repro",
            "1.20.1",
            LoaderSelection::vanilla(),
        ))
        .expect("create");
    registry.publish_client(&original).expect("publish");
    let _alongside =
        LaunchLock::acquire_additional(&original.root, "second").expect("an additional launch");

    let error = promote(&registry, original.id).expect_err("a held instance is not moved");

    assert!(
        matches!(error, RegistryError::Lock(LockError::AlreadyRunning { .. })),
        "{error}"
    );
    assert!(original.root.is_dir());
}
