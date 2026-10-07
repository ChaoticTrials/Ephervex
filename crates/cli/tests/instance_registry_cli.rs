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

mod support;

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
        support::seed_provider_metadata(directory.path());
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
    std::fs::remove_dir_all(temporary_root.parent().expect("temporary instances root"))
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
    assert!(temporary_instances.starts_with(sandbox.tmpdir()));
    assert_eq!(
        temporary_instances
            .file_name()
            .and_then(std::ffi::OsStr::to_str),
        Some("instances")
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
    for marker in ephervex_core::SECRET_KEY_MARKERS {
        assert!(!text.contains(marker), "configuration mentions `{marker}`");
    }

    // Nothing under the config root may contain a credential either.
    let config_root = sandbox.base().join("config").join("ephervex");
    if let Ok(entries) = std::fs::read_dir(&config_root) {
        for entry in entries.flatten() {
            let contents = std::fs::read_to_string(entry.path())
                .unwrap_or_default()
                .to_ascii_lowercase();
            for marker in ephervex_core::SECRET_KEY_MARKERS {
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
fn linked_server_creation_rejects_an_independent_version_selection() {
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

    let output = sandbox.run(&[
        "server",
        "create",
        "linked server",
        "--minecraft",
        "1.20.1",
        "--client",
        client_id,
        "--persistent",
    ]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "stderr was:\n{stderr}"
    );
    assert!(
        sandbox
            .json(&["--json", "servers", "list"])
            .as_array()
            .expect("array")
            .is_empty()
    );
}

#[test]
fn linked_server_creation_exposes_only_explicit_unknown_content_confirmation() {
    let sandbox = Sandbox::new();
    let help = sandbox.run(&["server", "create", "--help"]);
    assert!(help.status.success());
    let stdout = String::from_utf8_lossy(&help.stdout);
    assert!(stdout.contains("--client <CLIENT>"), "help was:\n{stdout}");
    assert!(
        stdout.contains("--include-unknown <PATH>"),
        "help was:\n{stdout}"
    );
    assert!(!stdout.contains("source archive"), "help was:\n{stdout}");
}

#[test]
fn client_launch_offers_one_linked_server_connection_option() {
    let sandbox = Sandbox::new();
    let help = sandbox.run(&["launch", "--help"]);
    assert!(help.status.success());
    let stdout = String::from_utf8_lossy(&help.stdout);
    assert!(stdout.contains("--server <SERVER>"), "help was:\n{stdout}");
    assert!(
        stdout.contains("Start or reuse this linked server"),
        "help was:\n{stdout}"
    );
}

/// Promoting a temporary instance moves it, rather than copying it, and the
/// instance keeps its identity (ADR 0025).
#[test]
fn persisting_an_instance_moves_it_under_the_persistent_root() {
    let sandbox = Sandbox::new();

    let created = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Repro",
        "--minecraft",
        "1.20.1",
    ]);
    let id = created["id"].as_str().expect("id").to_owned();
    let temporary_root = PathBuf::from(created["root"].as_str().expect("root"));
    assert_eq!(created["storage"], "temporary");
    assert!(temporary_root.starts_with(sandbox.tmpdir()));

    let promoted = sandbox.json(&["--json", "instance", "persist", &id]);

    assert_eq!(promoted["instance"], id, "the same instance, not a copy");
    assert_eq!(promoted["storage"], "persistent");
    let persistent_root = PathBuf::from(promoted["root"].as_str().expect("root"));
    assert!(
        persistent_root.starts_with(sandbox.base().join("data")),
        "expected the persistent data root, got {}",
        persistent_root.display()
    );
    assert!(
        !temporary_root.exists(),
        "a move leaves nothing behind in the temporary root"
    );
    assert!(persistent_root.join("minecraft").is_dir());

    // A separate process reads the same thing, and has nothing to reconcile.
    let listed = sandbox.json(&["--json", "instances", "list"]);
    let instances = listed.as_array().expect("array");
    assert_eq!(
        instances.len(),
        1,
        "promotion does not add a second instance"
    );
    assert_eq!(instances[0]["id"], id);
    assert_eq!(instances[0]["storage"], "persistent");

    let report = sandbox.json(&["--json", "reconcile"]);
    assert_eq!(report["needs_attention"], false);
    assert!(
        report["removed_stale_temporary"]
            .as_array()
            .expect("array")
            .is_empty()
    );
}

#[test]
fn persisting_an_instance_that_is_already_persistent_is_refused() {
    let sandbox = Sandbox::new();

    let created = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Kept",
        "--minecraft",
        "1.20.1",
        "--persistent",
    ]);
    let id = created["id"].as_str().expect("id").to_owned();

    let output = sandbox.run(&["instance", "persist", &id]);

    assert!(
        !output.status.success(),
        "an instance that is already persistent has nothing to promote"
    );
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(
        message.contains("already persistent"),
        "the refusal has to say why: {message}"
    );
}

#[test]
fn deleting_an_instance_with_its_files_needs_confirmation_and_reports_what_was_deleted() {
    let sandbox = Sandbox::new();
    let created = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Doomed repro",
        "--minecraft",
        "1.20.1",
    ]);
    let id = created["id"].as_str().expect("id").to_owned();
    let root = PathBuf::from(created["root"].as_str().expect("root"));

    let refused = sandbox.run(&["instance", "delete", &id]);
    assert!(
        !refused.status.success(),
        "no terminal to ask on, so nothing is deleted"
    );
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(
        message.contains("--yes"),
        "the refusal says how to confirm: {message}"
    );
    assert!(
        message.contains("Doomed repro"),
        "the refusal names the instance: {message}"
    );
    assert!(root.exists(), "a refused deletion removes nothing");
    assert_eq!(
        sandbox
            .json(&["--json", "instances", "list"])
            .as_array()
            .expect("array")
            .len(),
        1
    );

    let deleted = sandbox.json(&["--json", "instance", "delete", "--yes", &id]);
    assert_eq!(deleted["deleted"], id.as_str());
    assert_eq!(deleted["files"], "removed");
    assert_eq!(deleted["servers"], serde_json::json!([]));
    assert!(!root.exists());
}

#[test]
fn deleting_an_instance_while_keeping_its_files_needs_no_confirmation() {
    let sandbox = Sandbox::new();
    let created = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Kept files",
        "--minecraft",
        "1.20.1",
    ]);
    let id = created["id"].as_str().expect("id").to_owned();
    let root = PathBuf::from(created["root"].as_str().expect("root"));

    let deleted = sandbox.json(&["--json", "instance", "delete", "--keep-files", &id]);

    assert_eq!(deleted["files"], "kept");
    assert!(root.exists(), "the files stay on disk");
    assert!(
        sandbox
            .json(&["--json", "instances", "list"])
            .as_array()
            .expect("array")
            .is_empty()
    );
}

#[test]
fn reconcile_names_an_instance_claimed_by_two_directories_in_both_output_modes() {
    let sandbox = Sandbox::new();
    let created = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Repro",
        "--minecraft",
        "1.20.1",
        "--persistent",
    ]);
    let id = created["id"].as_str().expect("id").to_owned();
    let root = PathBuf::from(created["root"].as_str().expect("root"));
    let copy = root.parent().expect("instances root").join("copy");
    std::fs::create_dir_all(copy.join(".launcher")).expect("create copy");
    std::fs::copy(
        root.join(".launcher").join("instance.json"),
        copy.join(".launcher").join("instance.json"),
    )
    .expect("copy metadata");

    let text = sandbox.run(&["reconcile"]);
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains("claimed by more than one directory"),
        "{text}"
    );
    assert!(text.contains(&id), "{text}");
    assert!(text.contains(&copy.display().to_string()), "{text}");
    assert!(text.contains(&root.display().to_string()), "{text}");

    let report = sandbox.json(&["--json", "reconcile"]);
    assert_eq!(report["needs_attention"], true);
    let conflicts = report["client_conflicts"].as_array().expect("array");
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0]["id"], id);
    assert_eq!(conflicts[0]["roots"].as_array().expect("roots").len(), 2);
    assert!(
        report["server_conflicts"]
            .as_array()
            .expect("array")
            .is_empty()
    );
}
