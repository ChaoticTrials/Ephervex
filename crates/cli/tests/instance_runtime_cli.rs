//! `ephervex instance runtime` regression coverage, driven through the `ephervex` binary.
//!
//! Every run is pointed at a throwaway directory through the XDG variables and `TMPDIR`, set on a
//! *child* process, so nothing here touches the developer's real instances and nothing here needs
//! the network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
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
        // Java detection is off, so reading a host's runtime status never
        // probes the runtimes installed on the machine running the tests.
        let config = directory.path().join("config/ephervex");
        std::fs::create_dir_all(&config).expect("config dir");
        std::fs::write(
            config.join("config.toml"),
            "schema_version = 1\n\n[java]\nauto_detect = false\nauto_download = false\n",
        )
        .expect("write config");
        Self { directory }
    }

    /// An executable that answers the property probe as a JVM of `version`
    /// would.
    fn fake_java(&self, name: &str, version: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let home = self.directory.path().join(name);
        let executable = home.join("bin/java");
        std::fs::create_dir_all(executable.parent().expect("bin")).expect("bin dir");
        let arch = if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "amd64"
        };
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\ncat >&2 <<'EOF'\nProperty settings:\n    java.home = {home}\n    \
                 java.runtime.version = {version}+7\n    java.vendor = Eclipse Adoptium\n    \
                 java.version = {version}\n    os.arch = {arch}\nEOF\n",
                home = home.display(),
            ),
        )
        .expect("write java");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("make java executable");
        executable
    }

    fn run_in(&self, working: &Path, args: &[&str]) -> Output {
        let base = self.directory.path();
        binary_sandbox::ephervex_command()
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
    let mut shown = sandbox.json(&["--json", "instance", "runtime", &id]);
    assert!(shown["status"].is_object(), "{shown}");
    shown.as_object_mut().expect("an object").remove("status");
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

/// Rewrites a client's `instance.json` and leaves its SQLite index row as it was, which is what
/// remains after a committed change whose index write failed.
fn commit_metadata_only(root: &str, change: impl FnOnce(&mut serde_json::Value)) {
    let path = Path::new(root).join(".launcher").join("instance.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read the metadata"))
            .expect("the metadata is JSON");
    change(&mut document);
    std::fs::write(&path, serde_json::to_vec(&document).expect("serialize")).expect("write");
}

#[test]
fn a_runtime_option_derives_from_the_committed_pins_when_the_index_row_lags() {
    let sandbox = Sandbox::new();
    let record = sandbox.create_record();
    let id = record["id"].as_str().expect("an id").to_owned();
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
    commit_metadata_only(record["root"].as_str().expect("a root"), |document| {
        document["java_override"] = serde_json::json!({ "path": "/opt/jdk-21/bin/java" });
        document["jvm_override"] = serde_json::json!({
            "min_memory_mib": 2048,
            "max_memory_mib": 4096,
            "jvm_args": ["-Dbar=2"],
        });
    });

    let shown = sandbox.json(&["--json", "instance", "runtime", &id]);
    assert_eq!(shown["java"], "/opt/jdk-21/bin/java");
    assert_eq!(shown["min_memory_mib"], 2048);
    assert_eq!(shown["max_memory_mib"], 4096);
    assert_eq!(shown["jvm_args"], serde_json::json!(["-Dbar=2"]));

    let changed = sandbox.json(&["--json", "instance", "runtime", &id, "--max-memory", "3072"]);
    assert_eq!(changed["java"], "/opt/jdk-21/bin/java");
    assert_eq!(changed["min_memory_mib"], 2048);
    assert_eq!(changed["max_memory_mib"], 3072);
    assert_eq!(changed["jvm_args"], serde_json::json!(["-Dbar=2"]));
}

#[test]
fn reading_the_runtime_also_reports_which_java_runs_what_is_expected_and_whether_they_fit() {
    let sandbox = Sandbox::new();
    let id = sandbox.create();
    let java = sandbox.fake_java("jdk-17", "17.0.12");
    let java = java.to_str().expect("a UTF-8 path");
    let changed = sandbox.json(&["--json", "instance", "runtime", &id, "--java", java]);
    assert!(
        changed.get("status").is_none(),
        "a change does not read the status: {changed}"
    );

    let shown = sandbox.json(&["--json", "instance", "runtime", &id]);

    let status = &shown["status"];
    assert_eq!(status["status"], "customized");
    assert_eq!(status["mode"], "host");
    assert_eq!(status["runtime"]["major"], 17);
    assert_eq!(status["runtime"]["version"], "17.0.12");
    assert_eq!(status["runtime"]["vendor"], "Eclipse Adoptium");
    assert_eq!(status["runtime"]["managed"], false);
    assert_eq!(status["runtime"]["source"], "user-supplied");
    assert_eq!(status["expected"]["major"], 17);
    assert!(status["expected"]["bounded_by"].is_null());
    assert_eq!(status["problems"], serde_json::json!([]));

    let text = sandbox.run_in(sandbox.directory.path(), &["instance", "runtime", &id]);
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains("runtime:    java 17.0.12 (Eclipse Adoptium, ")
            && text.contains("(pinned by this host)")
            && text.contains("expected:   java 17")
            && text.contains("status:     customized"),
        "{text}"
    );
}

#[test]
fn a_pin_of_the_wrong_major_reads_as_a_problem_with_its_mismatch() {
    let sandbox = Sandbox::new();
    let id = sandbox.create();
    let java = sandbox.fake_java("jdk-21", "21.0.8");
    let java = java.to_str().expect("a UTF-8 path");
    sandbox.json(&["--json", "instance", "runtime", &id, "--java", java]);

    let status = sandbox.json(&["--json", "instance", "runtime", &id])["status"].clone();

    assert_eq!(status["status"], "problem");
    assert_eq!(status["problems"][0]["kind"], "incompatible");
    assert_eq!(status["problems"][0]["layer"], "host");
    assert_eq!(
        status["problems"][0]["mismatch"],
        serde_json::json!({"kind": "major", "required": 17, "found": 21})
    );
    assert_eq!(status["install"]["major"], 17);

    let text = sandbox.run_in(sandbox.directory.path(), &["instance", "runtime", &id]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains(&format!(
            "install:    ephervex java install --instance {id} (java 17)"
        )),
        "the install line names the host, so the loader's limits apply:\n{text}"
    );
}

#[test]
fn java_install_takes_a_version_or_one_host() {
    let sandbox = Sandbox::new();
    let id = sandbox.create();

    for args in [
        vec!["java", "install"],
        vec!["java", "install", "21", "--instance", &id],
        vec![
            "java",
            "install",
            "--instance",
            &id,
            "--server",
            "00000000-0000-4000-8000-000000000000",
        ],
    ] {
        let output = sandbox.run_in(sandbox.directory.path(), &args);
        assert!(!output.status.success(), "{args:?}");
        assert_eq!(output.status.code(), Some(2), "a usage error: {args:?}");
    }

    let help = sandbox.run_in(sandbox.directory.path(), &["java", "install", "--help"]);
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(
        help.contains("--instance") && help.contains("--server"),
        "{help}"
    );
}

#[test]
fn server_runtime_help_mirrors_instance_runtime_and_names_the_stopped_server_rule() {
    let sandbox = Sandbox::new();

    let help = sandbox.run_in(sandbox.directory.path(), &["server", "runtime", "--help"]);
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    for option in [
        "--java",
        "--min-memory",
        "--max-memory",
        "--jvm-arg",
        "--clear",
    ] {
        assert!(text.contains(option), "{option} missing from:\n{text}");
    }
    assert!(text.contains("stopped"), "help was:\n{text}");
}

#[test]
fn server_runtime_refuses_a_server_that_does_not_exist() {
    let sandbox = Sandbox::new();

    let output = sandbox.run_in(
        sandbox.directory.path(),
        &[
            "server",
            "runtime",
            "00000000-0000-4000-8000-000000000000",
            "--min-memory",
            "1024",
        ],
    );

    assert!(!output.status.success());
}

#[test]
fn java_select_takes_a_server_or_an_instance_but_not_both() {
    let sandbox = Sandbox::new();
    let id = sandbox.create();

    let output = sandbox.run_in(
        sandbox.directory.path(),
        &[
            "java",
            "select",
            "1.20.1",
            "--instance",
            &id,
            "--server",
            "00000000-0000-4000-8000-000000000000",
        ],
    );

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot be used with"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
