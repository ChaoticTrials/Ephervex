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

use ephervex_model::{AttentionReason, LoaderKind, LoaderSelection, StorageClass};
use ephervex_storage::{Config, NewClient, Paths, Registry, SECRET_KEY_MARKERS, config, reconcile};
use tempfile::TempDir;

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
    let temporary = {
        let registry = Registry::open(paths.clone()).expect("open registry");
        registry
            .create_client(NewClient::new(
                "Temp repro",
                "1.20.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "47.1.106"),
            ))
            .expect("create temporary instance")
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
        registry
            .create_client(
                NewClient::new(
                    "Kept repro",
                    "1.12.2",
                    LoaderSelection::exact(LoaderKind::Forge, "14.23.5.2859"),
                )
                .with_storage(StorageClass::Persistent),
            )
            .expect("create persistent instance")
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
