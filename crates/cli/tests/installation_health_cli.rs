//! `ephervex doctor` regression coverage, driven through the binary.
//!
//! Every run is pointed at a throwaway directory through the XDG environment
//! variables and `TMPDIR`, so nothing here reads or writes the developer's own
//! configuration (ADR 0003). The session bus is pointed at a socket that does
//! not exist, so no run reaches the developer's Secret Service (ADR 0004).
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

    fn base(&self) -> &Path {
        self.directory.path()
    }

    fn tmpdir(&self) -> PathBuf {
        self.base().join("tmp")
    }

    fn run(&self, args: &[&str]) -> Output {
        let base = self.base();
        binary_sandbox::ephervex_command()
            .args(args)
            .env("HOME", base)
            .env("XDG_CONFIG_HOME", base.join("config"))
            .env("XDG_DATA_HOME", base.join("data"))
            .env("XDG_CACHE_HOME", base.join("cache"))
            .env("XDG_STATE_HOME", base.join("state"))
            .env("TMPDIR", self.tmpdir())
            .env("EPHERVEX_LOG", "error")
            .output()
            .expect("run the ephervex binary")
    }

    /// Runs a command that must report a healthy installation, so a non-zero
    /// status is a test failure.
    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "`ephervex {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        Self::parse(args, &output)
    }

    /// Runs `doctor` against an installation that is expected to be faulty, and
    /// asserts the exit status says so. `doctor` still prints its whole report.
    fn faulty_json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.run(args);
        assert!(
            !output.status.success(),
            "`ephervex {}` reported a fault but exited successfully",
            args.join(" ")
        );
        Self::parse(args, &output)
    }

    fn parse(args: &[&str], output: &Output) -> serde_json::Value {
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "`ephervex {}` did not print JSON ({error}):\n{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }
}

/// Whether file permissions can deny this process at all.
///
/// Root ignores the mode bits the tests below set, so the states they are
/// written to produce cannot occur there and the assertions would invert. This
/// probes the property rather than asking who the process is, because the
/// property is what the tests depend on.
fn permissions_are_enforced() -> bool {
    let Ok(directory) = tempfile::tempdir() else {
        return false;
    };
    let blocked = directory.path().join("blocked");
    if std::fs::create_dir(&blocked).is_err() {
        return false;
    }
    set_mode(&blocked, 0o000);
    let denied = std::fs::read_dir(&blocked).is_err();
    set_mode(&blocked, 0o755);
    denied
}

#[test]
fn doctor_reports_a_fresh_installation_without_contacting_anything() {
    let sandbox = Sandbox::new();
    let report = sandbox.json(&["--json", "doctor"]);

    assert_eq!(report["application"]["app_id"], "ephervex");
    assert_eq!(report["application"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["platform"]["os"], std::env::consts::OS);
    assert!(report["paths_error"].is_null());

    // The bundled documents are the ones this build compiled in, so a broken
    // release artifact is visible here rather than at first use.
    assert_eq!(
        report["bundled_data"]["options_compatibility"]["status"],
        "loaded"
    );
    assert!(report["bundled_data"]["options_compatibility"]["entries"].as_u64() > Some(0));
    assert_eq!(
        report["bundled_data"]["preference_catalog"]["status"],
        "loaded"
    );
    assert!(report["bundled_data"]["preference_catalog"]["entries"].as_u64() > Some(0));

    // A first run has written no configuration, and the report must not create
    // one to be able to say so.
    assert_eq!(report["configuration"]["status"], "absent");
    assert!(!sandbox.base().join("config/ephervex/config.toml").exists());

    // Every directory the report names is inside the sandbox, except the
    // temporary root, which follows TMPDIR.
    let directories = report["directories"].as_array().expect("directories");
    assert!(!directories.is_empty());
    for directory in directories {
        let path = directory["path"].as_str().expect("path");
        assert!(
            path.starts_with(sandbox.base().to_str().expect("utf-8 base")),
            "doctor named a directory outside the sandbox: {path}"
        );
        // Every root of a fresh sandbox is either untouched or writable. A
        // third answer here would mean the probe could not tell the difference.
        assert!(
            directory["state"] == "absent" || directory["state"] == "writable",
            "doctor reported an unexpected state for a fresh root: {directory}"
        );
    }
}

#[test]
fn doctor_reports_the_credential_backend_without_reporting_a_credential() {
    let sandbox = Sandbox::new();
    let report = sandbox.json(&["--json", "doctor"]);

    let credentials = &report["credential_storage"];
    // With no session bus to reach, the answer is the same on every machine.
    assert_eq!(credentials["available"], false);
    assert_eq!(credentials["backend"], "session-only");
    assert!(
        credentials["detail"].is_string(),
        "an unavailable backend carries the platform's explanation"
    );
    // Availability is a fact about the machine. What is stored, and what it is,
    // are not part of this report at all (ADR 0004).
    //
    // Secret Service is the name of the desktop facility whose reachability is
    // being reported, and a machine without one says so in those words. Its
    // name is removed before the scan, which is otherwise looking for stored
    // material and is case-insensitive so that casing cannot hide a marker.
    let rendered = credentials
        .to_string()
        .to_lowercase()
        .replace("secret service", "");
    for marker in ["token", "secret", "password", "api_key", "credential_value"] {
        assert!(
            !rendered.contains(marker),
            "the credential section must not name {marker}: {rendered}"
        );
    }
}

#[test]
fn doctor_leaves_nothing_behind_in_the_directories_it_probes() {
    let sandbox = Sandbox::new();
    assert!(sandbox.run(&["doctor"]).status.success());

    // Writability is proven by writing, so the probe file must be gone again.
    let mut leftovers = Vec::new();
    let mut stack = vec![sandbox.base().to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| name.contains("write-probe"))
            {
                leftovers.push(path);
            }
        }
    }
    assert!(leftovers.is_empty(), "probe files survived: {leftovers:?}");
}

#[test]
fn doctor_answers_even_when_the_configuration_cannot_be_read() {
    let sandbox = Sandbox::new();
    let config = sandbox.base().join("config/ephervex/config.toml");
    std::fs::create_dir_all(config.parent().expect("config dir")).expect("create config dir");
    std::fs::write(&config, "schema_version = 1\n[java\n").expect("write a truncated file");

    // The whole point: a broken installation is what this command is for, so it
    // must succeed while reporting the failure rather than failing itself.
    let report = sandbox.faulty_json(&["--json", "doctor"]);
    assert_eq!(report["configuration"]["status"], "unusable");
    assert_eq!(
        report["configuration"]["message_id"],
        "error-config-invalid"
    );
    // The report says where and what, and still describes the rest of the
    // installation.
    assert!(
        !report["directories"]
            .as_array()
            .expect("directories")
            .is_empty()
    );
    assert_eq!(
        report["bundled_data"]["options_compatibility"]["status"],
        "loaded"
    );

    // The unreadable file is left exactly as it was.
    assert_eq!(
        std::fs::read_to_string(&config).expect("config"),
        "schema_version = 1\n[java\n"
    );
}

#[test]
fn doctor_refuses_a_configuration_from_a_newer_build_rather_than_guessing() {
    let sandbox = Sandbox::new();
    let config = sandbox.base().join("config/ephervex/config.toml");
    std::fs::create_dir_all(config.parent().expect("config dir")).expect("create config dir");
    std::fs::write(&config, "schema_version = 99999\n").expect("write");

    let report = sandbox.faulty_json(&["--json", "doctor"]);
    assert_eq!(report["configuration"]["status"], "unusable");
    assert_eq!(
        report["configuration"]["message_id"],
        "error-config-unsupported-schema"
    );
}

#[test]
fn a_configuration_that_cannot_be_read_is_reported_as_unusable_rather_than_missing() {
    let sandbox = Sandbox::new();
    if !permissions_are_enforced() {
        return;
    }
    let directory = sandbox.base().join("config/ephervex");
    std::fs::create_dir_all(&directory).expect("create config dir");
    std::fs::write(directory.join("config.toml"), "schema_version = 1\n").expect("write");
    // A directory nothing may look inside. Reading the file below fails with a
    // permission error rather than with `NotFound`.
    set_mode(&directory, 0o000);

    let report = sandbox.faulty_json(&["--json", "doctor"]);

    // The file is there. Reporting it absent would tell the reader their
    // settings had never been written, sending them to recreate what they
    // already have instead of fixing the permissions.
    assert_eq!(report["configuration"]["status"], "unusable");
    assert_eq!(
        report["configuration"]["message_id"],
        "error-config-unreadable"
    );

    set_mode(&directory, 0o755);
}

#[test]
fn a_storage_root_that_is_not_a_directory_is_reported_as_unusable() {
    let sandbox = Sandbox::new();
    // A regular file exactly where the data root belongs. Nothing will be
    // created here on first use, so calling it missing is wrong.
    std::fs::write(sandbox.base().join("data/ephervex"), b"not a directory").expect("write");

    let report = sandbox.faulty_json(&["--json", "doctor"]);

    let data = directory_named("data", &report);
    assert_eq!(data["state"], "unusable", "{data}");
    assert!(data["detail"].is_string(), "the reason is reported: {data}");
}

#[test]
fn a_storage_root_that_refuses_writes_is_reported_with_the_reason() {
    let sandbox = Sandbox::new();
    if !permissions_are_enforced() {
        return;
    }
    let directory = sandbox.base().join("cache/ephervex");
    std::fs::create_dir_all(&directory).expect("create cache dir");
    set_mode(&directory, 0o555);

    let report = sandbox.faulty_json(&["--json", "doctor"]);

    let cache = directory_named("cache", &report);
    assert_eq!(cache["state"], "not_writable", "{cache}");
    assert!(
        cache["detail"].is_string(),
        "the reason is reported: {cache}"
    );

    set_mode(&directory, 0o755);
}

fn directory_named<'a>(purpose: &str, report: &'a serde_json::Value) -> &'a serde_json::Value {
    report["directories"]
        .as_array()
        .expect("directories")
        .iter()
        .find(|directory| directory["purpose"] == purpose)
        .unwrap_or_else(|| panic!("doctor reported no `{purpose}` directory"))
}

fn set_mode(directory: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = std::fs::metadata(directory)
        .expect("metadata")
        .permissions();
    permissions.set_mode(mode);
    std::fs::set_permissions(directory, permissions).expect("set permissions");
}
