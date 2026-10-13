//! Account status reporting, driven through the `ephervex` binary.
//!
//! An offline account is a default account but not a signed-in one: it proves
//! nothing to Mojang and cannot join an online-mode server. Every run is pointed
//! at a throwaway directory through the XDG variables and `TMPDIR`, set on a
//! child process, and needs neither the network nor a keyring.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Output;

use tempfile::TempDir;

mod binary_sandbox;

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

    fn run(&self, args: &[&str]) -> Output {
        let base: &Path = self.directory.path();
        let output = binary_sandbox::ephervex_command()
            .args(args)
            .env("HOME", base)
            .env("XDG_CONFIG_HOME", base.join("config"))
            .env("XDG_DATA_HOME", base.join("data"))
            .env("XDG_CACHE_HOME", base.join("cache"))
            .env("XDG_STATE_HOME", base.join("state"))
            .env("TMPDIR", base.join("tmp"))
            .env("EPHERVEX_LOG", "error")
            .output()
            .expect("run the ephervex binary");
        assert!(
            output.status.success(),
            "`ephervex {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn text(&self, args: &[&str]) -> String {
        String::from_utf8(self.run(args).stdout).expect("utf-8 output")
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        serde_json::from_slice(&self.run(args).stdout).expect("JSON output")
    }
}

#[test]
fn an_offline_default_account_is_not_reported_as_signed_in_in_json() {
    let sandbox = Sandbox::new();
    sandbox.run(&["account", "add-offline", "Alice"]);

    let status = sandbox.json(&["--json", "account", "status"]);

    assert_eq!(status["signed_in"], false, "{status}");
    assert_eq!(status["default"]["name"], "Alice");
    assert_eq!(status["default"]["kind"], "offline");
    assert_eq!(status["default"]["authenticated"], false);
}

#[test]
fn text_and_json_status_agree_that_an_offline_account_is_not_signed_in() {
    let sandbox = Sandbox::new();
    sandbox.run(&["account", "add-offline", "Alice"]);

    let printed = sandbox.text(&["account", "status"]);

    assert!(printed.contains("Using the offline account"), "{printed}");
    assert_eq!(
        sandbox.json(&["--json", "account", "status"])["signed_in"],
        false
    );
}

#[test]
fn status_json_reports_no_accounts_as_not_signed_in() {
    let sandbox = Sandbox::new();

    let status = sandbox.json(&["--json", "account", "status"]);

    assert_eq!(status["signed_in"], false, "{status}");
    assert!(status["default"].is_null(), "{status}");
}
