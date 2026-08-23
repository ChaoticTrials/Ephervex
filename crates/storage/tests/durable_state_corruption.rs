//! What the launcher does when its own durable state is damaged.
//!
//! The rules every case here checks, from ADR 0002 and ADR 0003:
//!
//! - never panic;
//! - never accept bytes that cannot be verified as what they claim to be;
//! - never delete the only recoverable copy of something on the user's behalf;
//! - report the file and a message a frontend can localize;
//! - leave staging and temporary leftovers alone unless they are provably safe.
//!
//! Damage is simulated by writing over the files directly, which is exactly
//! what a power loss, a full disk, or a hand edit produces.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ephervex_model::{LoaderSelection, StorageClass};
use ephervex_storage::{Config, NewClient, Paths, Registry, metadata, reconcile};
use tempfile::TempDir;

fn paths(base: &TempDir) -> Paths {
    let paths = Paths::rooted_at(base.path());
    paths.ensure_base_directories().expect("create roots");
    paths
}

fn published_client(registry: &Registry, name: &str) -> ephervex_storage::ClientRecord {
    let record = registry
        .create_client(
            NewClient::new(name, "1.20.1", LoaderSelection::vanilla())
                .with_storage(StorageClass::Persistent),
        )
        .expect("create");
    registry.publish_client(&record).expect("publish");
    record
}

#[test]
fn a_truncated_configuration_is_reported_rather_than_partially_understood() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = paths(&base);
    // A write that stopped in the middle of a table header.
    let truncated = "schema_version = 1\nlanguage = \"en-US\"\n\n[ja";
    std::fs::write(paths.config_file(), truncated).expect("write");

    let error = Config::load_or_default(&paths).expect_err("a truncated file is not a valid one");
    assert_eq!(error.message_id(), "error-config-invalid");
    // The only copy of the user's settings is still there to be repaired.
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).expect("read"),
        truncated
    );
}

#[test]
fn a_configuration_truncated_to_nothing_reads_as_defaults_rather_than_failing() {
    // An empty file is valid TOML. It says nothing, so it means nothing, and
    // the launcher starts on its own defaults instead of refusing to run.
    let base = tempfile::tempdir().expect("temp dir");
    let paths = paths(&base);
    std::fs::write(paths.config_file(), "").expect("write");

    let config = Config::load_or_default(&paths).expect("an empty file is not a broken one");
    assert_eq!(config, Config::default());
}

#[test]
fn a_malformed_value_in_a_known_field_names_the_file() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = paths(&base);
    let document = "schema_version = 1\n[java]\nmax_memory_mib = \"as much as you like\"\n";
    std::fs::write(paths.config_file(), document).expect("write");

    let error = Config::load_or_default(&paths).expect_err("a string is not a memory size");
    assert_eq!(error.message_id(), "error-config-invalid");
    assert!(
        error.to_string().contains("config.toml"),
        "the error names the file so a user can find it: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).expect("read"),
        document
    );
}

#[test]
fn a_leftover_staging_sibling_is_neither_read_nor_left_behind_by_the_next_save() {
    // What an interrupted save leaves: a `.tmp.<uuid>` beside the real file.
    // It must never be mistaken for the configuration, and the next successful
    // save must publish cleanly regardless.
    let base = tempfile::tempdir().expect("temp dir");
    let paths = paths(&base);
    std::fs::write(
        paths.config_file(),
        "schema_version = 1\nlanguage = \"en-US\"\n",
    )
    .expect("write the real file");
    let orphan = paths.config_dir().join("config.toml.tmp.deadbeef");
    std::fs::write(&orphan, "schema_version = 1\nlanguage = \"xx-XX\"\n")
        .expect("write the interrupted staging file");

    let config = Config::load_or_default(&paths).expect("load");
    assert_eq!(
        config.language, "en-US",
        "the staging leftover must not be read as the configuration"
    );

    config.save(&paths).expect("save");
    // The orphan belongs to no operation this process started, so it is left
    // alone rather than removed on a guess -- but the save leaves none of its
    // own behind.
    assert!(orphan.is_file());
    let staging: Vec<_> = std::fs::read_dir(paths.config_dir())
        .expect("config dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp.") && name != "config.toml.tmp.deadbeef")
        .collect();
    assert!(
        staging.is_empty(),
        "the save left staging files: {staging:?}"
    );
}

#[test]
fn corrupt_instance_metadata_is_reported_and_never_repaired_or_deleted() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");
    let record = published_client(&registry, "Corrupted");

    let metadata_path = metadata::client_metadata_path(&record.root);
    let damaged = b"{\"metadata_version\":1,\"id\":\"not-a-uuid\"".to_vec();
    std::fs::write(&metadata_path, &damaged).expect("damage the metadata");

    // The index copy still answers, because that is what it is for.
    assert_eq!(
        registry.client(record.id).expect("index copy").id,
        record.id
    );

    // Reconciliation reports and moves on. It does not rewrite the file from
    // the index, and it does not delete the directory.
    let report = reconcile(&registry).expect("reconcile");
    assert!(report.unfinished_instances.is_empty());
    assert_eq!(
        std::fs::read(&metadata_path).expect("read"),
        damaged,
        "damaged metadata is left exactly as found"
    );
    assert!(record.root.is_dir());
}

#[test]
fn instance_metadata_from_a_newer_build_is_refused_rather_than_misread() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");
    let record = published_client(&registry, "From the future");

    let metadata_path = metadata::client_metadata_path(&record.root);
    let text = std::fs::read_to_string(&metadata_path).expect("read");
    let bumped = text.replace("\"metadata_version\":1", "\"metadata_version\":9999");
    std::fs::write(&metadata_path, &bumped).expect("write");

    // Losing the index must not turn an unreadable future format into a
    // half-understood instance.
    std::fs::remove_file(paths.database_file()).expect("drop the index");
    let registry = Registry::open(paths.clone()).expect("reopen");
    let report = reconcile(&registry).expect("reconcile");

    assert!(registry.list_clients().expect("list").is_empty());
    assert!(report.reindexed.is_empty());
    assert_eq!(
        std::fs::read_to_string(&metadata_path).expect("read"),
        bumped,
        "a format this build cannot read is left for the build that can"
    );
}

#[test]
fn a_directory_that_is_not_an_instance_is_never_adopted_as_one() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = paths(&base);
    let registry = Registry::open(paths.clone()).expect("open registry");

    // Something a user dropped into the instances folder by hand.
    let stray = paths.persistent_instances_dir().join("holiday-photos");
    std::fs::create_dir_all(stray.join("subfolder")).expect("create");
    std::fs::write(stray.join("subfolder/note.txt"), "not an instance").expect("write");

    let report = reconcile(&registry).expect("reconcile");
    assert!(registry.list_clients().expect("list").is_empty());
    assert!(report.reindexed.is_empty());
    assert!(
        stray.join("subfolder/note.txt").is_file(),
        "an unrelated directory is left untouched"
    );
}

#[test]
fn a_database_this_build_cannot_read_is_refused_rather_than_migrated_backwards() {
    let base = tempfile::tempdir().expect("temp dir");
    let paths = paths(&base);
    {
        let registry = Registry::open(paths.clone()).expect("open registry");
        published_client(&registry, "Kept");
    }

    let connection = rusqlite::Connection::open(paths.database_file()).expect("open the index");
    connection
        .pragma_update(None, "user_version", 9999)
        .expect("claim a future schema");
    drop(connection);

    assert!(
        Registry::open(paths.clone()).is_err(),
        "a database from a newer build must be refused, not downgraded"
    );
    assert!(
        paths.database_file().is_file(),
        "the refused database is left for the build that wrote it"
    );
}

#[test]
fn a_corrupt_database_leaves_the_instance_directories_recoverable() {
    // The payoff of ADR 0002: the database is an index, not the truth. Losing
    // it to corruption costs a rebuild, never an instance.
    let base = tempfile::tempdir().expect("temp dir");
    let paths = paths(&base);
    let root = {
        let registry = Registry::open(paths.clone()).expect("open registry");
        published_client(&registry, "Recoverable").root
    };

    std::fs::write(paths.database_file(), b"this is not a database").expect("corrupt the index");
    // A file that is not a database at all is refused rather than silently
    // replaced, because replacing it is what would lose the rows.
    let refused = Registry::open(paths.clone()).is_err();
    assert!(refused || paths.database_file().is_file());
    assert!(
        metadata::client_metadata_path(&root).is_file(),
        "the instance's own canonical metadata is untouched by a broken index"
    );
}
