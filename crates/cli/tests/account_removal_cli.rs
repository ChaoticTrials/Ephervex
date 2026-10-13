//! Account removal reporting, driven through the `ephervex` binary.
//!
//! An offline account has no credential, so removing it or signing out of it
//! must never claim one was deleted. Every run is pointed at a throwaway
//! directory through the XDG variables and `TMPDIR`, set on a child process, and
//! needs neither the network nor a keyring.
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
fn signing_out_of_an_offline_account_does_not_claim_a_credential_was_deleted() {
    let sandbox = Sandbox::new();
    sandbox.run(&["account", "add-offline", "Alice"]);

    let printed = sandbox.text(&["account", "logout"]);

    assert!(printed.contains("Signed out."), "{printed}");
    assert!(
        printed.contains("no saved credential"),
        "the output must say there was nothing to delete: {printed}"
    );
    assert!(!printed.contains("deleted the saved"), "{printed}");
}

#[test]
fn signing_out_reports_the_credential_outcome_in_json() {
    let sandbox = Sandbox::new();
    sandbox.run(&["account", "add-offline", "Alice"]);

    let signed_out = sandbox.json(&["--json", "account", "logout"]);
    assert_eq!(signed_out["signed_out"], true);
    assert_eq!(signed_out["credential"], "none-stored");

    let nobody = sandbox.json(&["--json", "account", "logout"]);
    assert_eq!(nobody["signed_out"], false);
    assert!(nobody["credential"].is_null(), "{nobody}");
}

#[test]
fn removing_an_offline_account_does_not_claim_a_credential_was_deleted() {
    let sandbox = Sandbox::new();
    sandbox.run(&["account", "add-offline", "Alice"]);

    let printed = sandbox.text(&["account", "remove", "Alice"]);

    assert!(printed.contains("Removed the account Alice."), "{printed}");
    assert!(!printed.contains("credential"), "{printed}");

    sandbox.run(&["account", "add-offline", "Bob"]);
    let removed = sandbox.json(&["--json", "account", "remove", "Bob"]);
    assert_eq!(removed["credential"], "none-stored");
}
