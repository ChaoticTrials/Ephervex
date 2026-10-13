//! Snapshot management exercised through fresh CLI processes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Output;

use tempfile::TempDir;

mod binary_sandbox;
mod support;

struct Sandbox {
    directory: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temp dir");
        for child in ["config", "data", "cache", "state", "tmp"] {
            std::fs::create_dir_all(directory.path().join(child)).expect("create root");
        }
        support::seed_provider_metadata(directory.path());
        Self { directory }
    }

    fn run(&self, args: &[&str]) -> Output {
        let base = self.directory.path();
        binary_sandbox::ephervex_command()
            .args(args)
            .env("HOME", base)
            .env("XDG_CONFIG_HOME", base.join("config"))
            .env("XDG_DATA_HOME", base.join("data"))
            .env("XDG_CACHE_HOME", base.join("cache"))
            .env("XDG_STATE_HOME", base.join("state"))
            .env("TMPDIR", base.join("tmp"))
            .env("EPHERVEX_LOG", "error")
            .output()
            .expect("run ephervex")
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "`ephervex {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "`ephervex {}` did not print JSON ({error}):\n{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }
}

fn create_instance(sandbox: &Sandbox) -> (String, PathBuf) {
    let created = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Snapshot CLI",
        "--minecraft",
        "1.20.1",
    ]);
    (
        created["id"].as_str().expect("instance id").to_owned(),
        PathBuf::from(created["root"].as_str().expect("instance root")),
    )
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    std::fs::write(path, bytes).expect("write file");
}

#[test]
fn the_cli_drives_the_complete_snapshot_lifecycle() {
    let sandbox = Sandbox::new();
    let (instance, root) = create_instance(&sandbox);
    let state = root.join("minecraft/state.txt");
    let added = root.join("minecraft/added-later.txt");
    write(&state, b"captured");

    let created = sandbox.json(&[
        "--json",
        "snapshot",
        "create",
        "--instance",
        &instance,
        "known good",
    ]);
    let snapshot = created["id"].as_str().expect("snapshot id").to_owned();
    assert_eq!(created["label"], "known good");

    let listed = sandbox.json(&["--json", "snapshot", "list", "--instance", &instance]);
    assert_eq!(listed.as_array().expect("snapshot list").len(), 1);

    write(&state, b"changed");
    write(&added, b"not in snapshot");
    let refused = sandbox.run(&["snapshot", "restore", "--instance", &instance, &snapshot]);
    assert!(!refused.status.success(), "restore requires confirmation");
    assert_eq!(std::fs::read(&state).expect("live state"), b"changed");

    let restored = sandbox.json(&[
        "--json",
        "snapshot",
        "restore",
        "--instance",
        &instance,
        "--no-safety-snapshot",
        "--yes",
        &snapshot,
    ]);
    assert_eq!(restored["restored"]["id"], snapshot);
    assert!(restored["safety_snapshot"].is_null());
    assert_eq!(std::fs::read(&state).expect("restored state"), b"captured");
    assert!(!added.exists(), "restore replaces rather than merges");

    let renamed = sandbox.json(&[
        "--json",
        "snapshot",
        "rename",
        "--instance",
        &instance,
        &snapshot,
        "reproduces crash",
    ]);
    assert_eq!(renamed["label"], "reproduces crash");

    let refused = sandbox.run(&["snapshot", "delete", "--instance", &instance, &snapshot]);
    assert!(!refused.status.success(), "delete requires confirmation");
    let deleted = sandbox.json(&[
        "--json",
        "snapshot",
        "delete",
        "--instance",
        &instance,
        "--yes",
        &snapshot,
    ]);
    assert_eq!(deleted["deleted"], snapshot);
    assert!(
        sandbox
            .json(&["--json", "snapshot", "list", "--instance", &instance])
            .as_array()
            .expect("snapshot list")
            .is_empty()
    );
}
