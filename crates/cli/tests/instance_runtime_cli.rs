//! `ephervex instance runtime` regression coverage, driven through the `ephervex` binary.
//!
//! Every run is pointed at a throwaway directory through the XDG variables and `TMPDIR`, set on a
//! *child* process, so nothing here touches the developer's real instances and nothing here needs
//! the network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

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

    fn run_in(&self, working: &Path, args: &[&str]) -> Output {
        let base = self.directory.path();
        Command::new(env!("CARGO_BIN_EXE_ephervex"))
            .args(args)
            .current_dir(working)
            .env("HOME", base)
            .env("XDG_CONFIG_HOME", base.join("config"))
            .env("XDG_DATA_HOME", base.join("data"))
            .env("XDG_CACHE_HOME", base.join("cache"))
            .env("XDG_STATE_HOME", base.join("state"))
            .env("TMPDIR", base.join("tmp"))
            .env("EPHERVEX_LOG", "error")
            .output()
            .expect("run the ephervex binary")
    }

    fn json_in(&self, working: &Path, args: &[&str]) -> serde_json::Value {
        let output = self.run_in(working, args);
        assert!(
            output.status.success(),
            "`ephervex {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("the command prints JSON")
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        self.json_in(self.directory.path(), args)
    }

    /// Creates a client instance and returns its id.
    fn create(&self) -> String {
        self.create_record()["id"]
            .as_str()
            .expect("an id")
            .to_owned()
    }

    fn create_record(&self) -> serde_json::Value {
        self.json(&[
            "--json",
            "instance",
            "create",
            "Runtime repro",
            "--minecraft",
            "1.20.1",
        ])
    }
}

#[test]
fn an_option_changes_only_its_own_value_and_keeps_the_other_pins() {
    let sandbox = Sandbox::new();
    let id = sandbox.create();
    sandbox.json(&[
        "--json",
        "instance",
        "runtime",
        &id,
        "--java",
        "/opt/jdk-17/bin/java",
        "--min-memory",
        "1024",
        "--jvm-arg=-Dfoo=1",
    ]);

    let after_memory =
        sandbox.json(&["--json", "instance", "runtime", &id, "--max-memory", "4096"]);
    assert_eq!(after_memory["java"], "/opt/jdk-17/bin/java");
    assert_eq!(after_memory["min_memory_mib"], 1024);
    assert_eq!(after_memory["max_memory_mib"], 4096);
    assert_eq!(after_memory["jvm_args"], serde_json::json!(["-Dfoo=1"]));

    let after_java = sandbox.json(&[
        "--json",
        "instance",
        "runtime",
        &id,
        "--java",
        "/opt/jdk-21/bin/java",
    ]);
    assert_eq!(after_java["java"], "/opt/jdk-21/bin/java");
    assert_eq!(after_java["min_memory_mib"], 1024);
    assert_eq!(after_java["max_memory_mib"], 4096);
    assert_eq!(after_java["jvm_args"], serde_json::json!(["-Dfoo=1"]));

    // `--jvm-arg` replaces the pinned list and nothing else.
    let after_args = sandbox.json(&["--json", "instance", "runtime", &id, "--jvm-arg=-Dbar=2"]);
    assert_eq!(after_args["java"], "/opt/jdk-21/bin/java");
    assert_eq!(after_args["max_memory_mib"], 4096);
    assert_eq!(after_args["jvm_args"], serde_json::json!(["-Dbar=2"]));

    // Reading back without options changes nothing, and `--clear` removes every pin.
    let shown = sandbox.json(&["--json", "instance", "runtime", &id]);
    assert_eq!(shown, after_args);
    let cleared = sandbox.json(&["--json", "instance", "runtime", &id, "--clear"]);
    assert!(cleared["java"].is_null());
    assert!(cleared["min_memory_mib"].is_null());
    assert!(cleared["max_memory_mib"].is_null());
    assert_eq!(cleared["jvm_args"], serde_json::json!([]));
}

#[test]
fn a_relative_java_path_is_pinned_as_the_absolute_path_it_named() {
    let sandbox = Sandbox::new();
    let id = sandbox.create();
    let working = tempfile::tempdir().expect("working directory");

    let pinned = sandbox.json_in(
        working.path(),
        &[
            "--json",
            "instance",
            "runtime",
            &id,
            "--java",
            "jdk-17/bin/java",
        ],
    );
    let stored = std::path::PathBuf::from(pinned["java"].as_str().expect("a java path"));
    assert!(stored.is_absolute(), "stored as `{}`", stored.display());
    assert!(stored.ends_with("jdk-17/bin/java"));

    // A later command from another directory sees the same Java.
    let elsewhere = sandbox.json(&["--json", "instance", "runtime", &id]);
    assert_eq!(elsewhere["java"], pinned["java"]);
}

#[test]
fn runtime_help_describes_the_jvm_argument_option_without_naming_parser_internals() {
    let sandbox = Sandbox::new();

    let help = sandbox.run_in(sandbox.directory.path(), &["instance", "runtime", "--help"]);
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);

    assert!(text.contains("--jvm-arg"), "help was:\n{text}");
    assert!(!text.contains("allow_hyphen_values"), "help was:\n{text}");
}

#[test]
fn a_saved_change_reports_no_warnings_when_the_metadata_directory_syncs() {
    let sandbox = Sandbox::new();
    let id = sandbox.create();

    let pinned = sandbox.json(&["--json", "instance", "runtime", &id, "--min-memory", "2048"]);

    assert_eq!(pinned["warnings"], serde_json::json!([]));
}

#[cfg(unix)]
#[test]
fn a_change_saved_without_syncing_its_metadata_directory_reports_it_on_both_outputs() {
    use std::os::unix::fs::PermissionsExt as _;

    let sandbox = Sandbox::new();
    let record = sandbox.create_record();
    let id = record["id"].as_str().expect("an id").to_owned();
    let metadata = Path::new(record["root"].as_str().expect("a root")).join(".launcher");
    std::fs::set_permissions(&metadata, std::fs::Permissions::from_mode(0o300))
        .expect("restrict the metadata directory");
    // A directory that opens anyway (running as root) can be synced, so there
    // is no failure to provoke.
    let unsyncable = std::fs::File::open(&metadata).is_err();
    let json = unsyncable.then(|| {
        sandbox.run_in(
            sandbox.directory.path(),
            &["--json", "instance", "runtime", &id, "--min-memory", "2048"],
        )
    });
    let text = unsyncable.then(|| {
        sandbox.run_in(
            sandbox.directory.path(),
            &["instance", "runtime", &id, "--min-memory", "3072"],
        )
    });
    std::fs::set_permissions(&metadata, std::fs::Permissions::from_mode(0o700))
        .expect("restore the metadata directory");
    let (Some(json), Some(text)) = (json, text) else {
        return;
    };

    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let printed: serde_json::Value = serde_json::from_slice(&json.stdout).expect("JSON");
    assert_eq!(printed["min_memory_mib"], 2048);
    assert_eq!(printed["warnings"][0]["kind"], "metadata_not_durable");
    assert!(printed["warnings"][0]["detail"].is_string());

    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    assert!(
        String::from_utf8_lossy(&text.stderr).contains("could not be synced to disk"),
        "the warning belongs on stderr: {}",
        String::from_utf8_lossy(&text.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&text.stdout).contains("synced"),
        "stdout keeps only the settings"
    );
}
