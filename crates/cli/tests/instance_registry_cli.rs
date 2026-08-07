//! The storage and registry support regression coverage driven through the `ephervex` binary.
//!
//! Every run is pointed at a throwaway directory through the XDG environment
//! variables and `TMPDIR`, which is also what makes these tests the honest
//! proof that path resolution reads the environment rather than hardcoding
//! anything: the variables are set on a *child process*, so nothing here has to
//! mutate this process's environment (ADR 0003).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// A disposable home directory for one test.
struct Sandbox {
    directory: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temp dir");
        for child in ["config", "data", "cache", "state", "tmp"] {
            std::fs::create_dir_all(directory.path().join(child)).expect("create root");
        }
        Self { directory }
    }

    fn base(&self) -> &Path {
        self.directory.path()
    }

    fn tmpdir(&self) -> PathBuf {
        self.base().join("tmp")
    }

    /// Runs `ephervex` with every root redirected into the sandbox.
    fn run(&self, args: &[&str]) -> Output {
        let base = self.base();
        Command::new(env!("CARGO_BIN_EXE_ephervex"))
            .args(args)
            .env("HOME", base)
            .env("XDG_CONFIG_HOME", base.join("config"))
            .env("XDG_DATA_HOME", base.join("data"))
            .env("XDG_CACHE_HOME", base.join("cache"))
            .env("XDG_STATE_HOME", base.join("state"))
            .env("TMPDIR", self.tmpdir())
            // Keep stderr quiet so a failing assertion is readable.
            .env("EPHERVEX_LOG", "error")
            .output()
            .expect("run the ephervex binary")
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

#[test]
fn the_cli_exercises_the_instance_registry_workflow() {
    let sandbox = Sandbox::new();

    // --- create temp record -------------------------------------------------
    let temporary = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Temp repro",
        "--minecraft",
        "1.20.1",
    ]);
    assert_eq!(
        temporary["storage"], "temporary",
        "temporary is the default"
    );

    // --- create persistent record -------------------------------------------
    let persistent = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Kept repro",
        "--minecraft",
        "1.12.2",
        "--loader",
        "forge",
        "--loader-version",
        "14.23.5.2859",
        "--persistent",
    ]);
    assert_eq!(persistent["storage"], "persistent");

    // --- restart registry ---------------------------------------------------
    // Each invocation is a fresh process reopening the same database.
    let listed = sandbox.json(&["--json", "instances", "list"]);
    assert_eq!(listed.as_array().expect("array").len(), 2);

    // --- reconcile missing temp directory -----------------------------------
    let temporary_root = PathBuf::from(temporary["root"].as_str().expect("root path"));
    assert!(
        temporary_root.starts_with(sandbox.tmpdir()),
        "the temporary instance must live under TMPDIR, not a hardcoded /tmp: {}",
        temporary_root.display()
    );
    std::fs::remove_dir_all(sandbox.tmpdir().join("ephervex").join("instances"))
        .expect("simulate the OS clearing its temporary directory");

    let report = sandbox.json(&["--json", "reconcile"]);
    assert_eq!(
        report["removed_stale_temporary"]
            .as_array()
            .expect("array")
            .len(),
        1
    );
    assert_eq!(
        report["missing_persistent"]
            .as_array()
            .expect("array")
            .len(),
        0
    );
    assert_eq!(report["needs_attention"], false);

    // --- list remaining records ---------------------------------------------
    let remaining = sandbox.json(&["--json", "instances", "list"]);
    let remaining = remaining.as_array().expect("array");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0]["id"], persistent["id"]);
    assert_eq!(remaining[0]["storage"], "persistent");
}

#[test]
fn the_temporary_instance_root_follows_tmpdir() {
    // The end-to-end proof that `std::env::temp_dir()` is what resolves the
    // temporary root. A hardcoded `/tmp` would fail this.
    let sandbox = Sandbox::new();
    let paths = sandbox.json(&["--json", "paths"]);

    let temporary_instances = PathBuf::from(paths["temporary_instances"].as_str().expect("path"));
    assert_eq!(
        temporary_instances,
        sandbox.tmpdir().join("ephervex").join("instances")
    );

    //...and the XDG roots follow their variables too.
    assert_eq!(
        PathBuf::from(paths["config"].as_str().expect("path")),
        sandbox.base().join("config").join("ephervex")
    );
    assert_eq!(
        PathBuf::from(paths["database"].as_str().expect("path")),
        sandbox
            .base()
            .join("state")
            .join("ephervex")
            .join("launcher.sqlite")
    );
}

#[test]
fn the_cli_writes_no_secret_into_the_configuration() {
    let sandbox = Sandbox::new();
    sandbox.run(&["instance", "create", "x", "--minecraft", "1.20.1"]);

    let rendered = sandbox.run(&["config"]);
    assert!(rendered.status.success());
    let text = String::from_utf8_lossy(&rendered.stdout).to_ascii_lowercase();
    for marker in ephervex_storage::SECRET_KEY_MARKERS {
        assert!(!text.contains(marker), "configuration mentions `{marker}`");
    }

    // Nothing under the config root may contain a credential either.
    let config_root = sandbox.base().join("config").join("ephervex");
    if let Ok(entries) = std::fs::read_dir(&config_root) {
        for entry in entries.flatten() {
            let contents = std::fs::read_to_string(entry.path())
                .unwrap_or_default()
                .to_ascii_lowercase();
            for marker in ephervex_storage::SECRET_KEY_MARKERS {
                assert!(
                    !contents.contains(marker),
                    "{:?} mentions `{marker}`",
                    entry.path()
                );
            }
        }
    }
}

#[test]
fn a_modded_loader_without_a_version_is_rejected_rather_than_guessed() {
    // never silently substitute a loader version.
    let sandbox = Sandbox::new();
    let output = sandbox.run(&[
        "instance",
        "create",
        "guessy",
        "--minecraft",
        "1.20.1",
        "--loader",
        "neoforge",
    ]);

    assert!(!output.status.success(), "must not create the instance");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--loader-version"), "stderr was:\n{stderr}");

    let listed = sandbox.json(&["--json", "instances", "list"]);
    assert!(listed.as_array().expect("array").is_empty());
}

#[test]
fn several_servers_can_link_to_one_client_through_the_cli() {
    let sandbox = Sandbox::new();
    let client = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "client",
        "--minecraft",
        "1.20.1",
        "--persistent",
    ]);
    let client_id = client["id"].as_str().expect("id");

    for name in ["server a", "server b"] {
        sandbox.json(&[
            "--json",
            "server",
            "create",
            name,
            "--minecraft",
            "1.20.1",
            "--client",
            client_id,
            "--persistent",
        ]);
    }

    let shown = sandbox.json(&["--json", "instance", "show", client_id]);
    assert_eq!(
        shown["linked_server_ids"].as_array().expect("array").len(),
        2
    );
    assert_eq!(
        sandbox
            .json(&["--json", "servers", "list"])
            .as_array()
            .expect("array")
            .len(),
        2
    );
}
