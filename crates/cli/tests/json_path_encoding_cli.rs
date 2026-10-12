//! `--json` output encodes paths that are not valid UTF-8 instead of aborting.
//!
//! JSON strings are UTF-8 and a Unix path is any byte string, so a directory
//! the user created (or an `XDG_*` variable they exported) with an invalid byte
//! must not take a machine-readable command down. Such bytes are written as
//! U+FFFD.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

mod support;

/// A directory name that is not valid UTF-8.
fn invalid_name(prefix: &str) -> OsString {
    let mut bytes = prefix.as_bytes().to_vec();
    bytes.push(0xff);
    OsString::from_vec(bytes)
}

struct Sandbox {
    directory: TempDir,
    data_root: PathBuf,
}

impl Sandbox {
    /// A sandbox whose `XDG_DATA_HOME` is `data_name` below the base.
    fn with_data_root(data_name: &OsStr) -> Self {
        let directory = tempfile::tempdir().expect("temp dir");
        let data_root = directory.path().join(data_name);
        for child in ["config", "cache", "state", "tmp"] {
            std::fs::create_dir_all(directory.path().join(child)).expect("create root");
        }
        std::fs::create_dir_all(&data_root).expect("create data root");
        support::seed_provider_metadata(directory.path());
        Self {
            directory,
            data_root,
        }
    }

    fn base(&self) -> &Path {
        self.directory.path()
    }

    fn run(&self, args: &[&str]) -> Output {
        let base = self.base();
        Command::new(env!("CARGO_BIN_EXE_ephervex"))
            .args(args)
            .env("HOME", base)
            .env("XDG_CONFIG_HOME", base.join("config"))
            .env("XDG_DATA_HOME", &self.data_root)
            .env("XDG_CACHE_HOME", base.join("cache"))
            .env("XDG_STATE_HOME", base.join("state"))
            .env("TMPDIR", base.join("tmp"))
            .env("EPHERVEX_LOG", "error")
            .output()
            .expect("run the ephervex binary")
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "`ephervex {}` exited with {:?}:\n{}",
            args.join(" "),
            output.status.code(),
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

fn text<'a>(value: &'a serde_json::Value, what: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{what} is a string"))
}

#[test]
fn paths_reports_a_data_root_that_is_not_valid_utf8() {
    let sandbox = Sandbox::with_data_root(&invalid_name("data"));

    let paths = sandbox.json(&["--json", "paths"]);

    for key in ["data", "persistent_instances", "persistent_servers"] {
        let value = text(&paths[key], key);
        assert!(
            value.contains('\u{fffd}'),
            "`{key}` keeps the invalid byte as U+FFFD: {value}"
        );
    }
}

#[test]
fn common_json_commands_survive_a_data_root_that_is_not_valid_utf8() {
    let sandbox = Sandbox::with_data_root(&invalid_name("data"));
    for args in [
        &["--json", "paths"][..],
        &["--json", "instances", "list"],
        &["--json", "reconcile"],
        &["--json", "config", "show"],
    ] {
        let output = sandbox.run(args);
        assert_ne!(
            output.status.code(),
            Some(101),
            "`ephervex {}` panicked:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).is_ok(),
            "`ephervex {}` printed no JSON document:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn creating_an_instance_under_a_data_root_that_is_not_valid_utf8_names_the_encoding() {
    let sandbox = Sandbox::with_data_root(&invalid_name("data"));

    let output = sandbox.run(&[
        "--json",
        "instance",
        "create",
        "Repro",
        "--minecraft",
        "1.20.1",
        "--persistent",
    ]);

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(r#""message_id": "error-paths-not-utf8""#),
        "the error names the encoding, not unwritable metadata:\n{stderr}"
    );
}

#[test]
fn reconcile_reports_conflicting_roots_whose_names_are_not_valid_utf8() {
    let sandbox = Sandbox::with_data_root(OsStr::new("data"));
    let created = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Repro",
        "--minecraft",
        "1.20.1",
        "--persistent",
    ]);
    let root = PathBuf::from(text(&created["root"], "root"));
    let copy = root
        .parent()
        .expect("instances root")
        .join(invalid_name("copy"));
    std::fs::create_dir_all(copy.join(".launcher")).expect("create copy");
    std::fs::copy(
        root.join(".launcher").join("instance.json"),
        copy.join(".launcher").join("instance.json"),
    )
    .expect("copy metadata");

    let report = sandbox.json(&["--json", "reconcile"]);

    let conflicts = report["client_conflicts"].as_array().expect("array");
    assert_eq!(conflicts.len(), 1);
    let roots: Vec<&str> = conflicts[0]["roots"]
        .as_array()
        .expect("roots")
        .iter()
        .map(|root| text(root, "root"))
        .collect();
    assert_eq!(roots.len(), 2);
    assert!(
        roots.iter().any(|root| root.contains('\u{fffd}')),
        "the copy's name keeps the invalid byte as U+FFFD: {roots:?}"
    );
}

#[test]
fn reconcile_reports_a_reindexed_root_whose_name_is_not_valid_utf8() {
    let origin = Sandbox::with_data_root(OsStr::new("data"));
    let created = origin.json(&[
        "--json",
        "instance",
        "create",
        "Repro",
        "--minecraft",
        "1.20.1",
        "--persistent",
    ]);
    let source = PathBuf::from(text(&created["root"], "root"));

    // A fresh registry that has never heard of the instance finds it on disk.
    let sandbox = Sandbox::with_data_root(OsStr::new("data"));
    let instances = PathBuf::from(text(
        &sandbox.json(&["--json", "paths"])["persistent_instances"],
        "persistent_instances",
    ));
    let moved = instances.join(invalid_name("moved"));
    std::fs::create_dir_all(moved.join(".launcher")).expect("create directory");
    std::fs::copy(
        source.join(".launcher").join("instance.json"),
        moved.join(".launcher").join("instance.json"),
    )
    .expect("copy metadata");

    let report = sandbox.json(&["--json", "reconcile"]);

    let reindexed = report["reindexed"].as_array().expect("array");
    assert_eq!(reindexed.len(), 1, "{report}");
    assert!(text(&reindexed[0]["root"], "root").contains('\u{fffd}'));
}

#[test]
fn a_path_that_is_valid_utf8_is_written_unchanged() {
    let sandbox = Sandbox::with_data_root(OsStr::new("data"));

    let paths = sandbox.json(&["--json", "paths"]);

    assert_eq!(
        Path::new(text(&paths["data"], "data")),
        sandbox.data_root.join("ephervex")
    );
}
