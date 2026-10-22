//! Attaching, reading, and removing an instance's issue link through the
//! `ephervex` binary.
//!
//! Every run is pointed at a throwaway directory through the XDG variables and
//! `TMPDIR`, and nothing here needs the network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Output;

use tempfile::TempDir;

mod binary_sandbox;
mod support;

const LINK: &str = "https://example.invalid/issues/42";

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

    fn run(&self, args: &[&str]) -> Output {
        let base = self.base();
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
            .expect("run the ephervex binary")
    }

    fn ok(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "`ephervex {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.ok(args);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "`ephervex {}` did not print JSON ({error}):\n{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn create(&self, name: &str) -> String {
        self.json(&[
            "--json",
            "instance",
            "create",
            name,
            "--minecraft",
            "1.20.1",
        ])["id"]
            .as_str()
            .expect("id")
            .to_owned()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn an_instance_without_an_issue_link_reports_none() {
    let sandbox = Sandbox::new();
    let id = sandbox.create("Alpha");

    let shown = sandbox.json(&["--json", "instance", "show", &id]);
    assert!(shown["issue_link"].is_null());
    assert!(!stdout(&sandbox.ok(&["instance", "show", &id])).contains("issue:"));

    let asked = sandbox.json(&["--json", "instance", "issue", &id]);
    assert!(asked["issue_link"].is_null());
    assert!(stdout(&sandbox.ok(&["instance", "issue", &id])).contains("no issue link"));
}

#[test]
fn an_issue_link_is_set_shown_listed_and_cleared() {
    let sandbox = Sandbox::new();
    let id = sandbox.create("Alpha");

    let set = sandbox.json(&["--json", "instance", "issue", &id, LINK]);
    assert_eq!(set["issue_link"], LINK);

    let shown = sandbox.json(&["--json", "instance", "show", &id]);
    assert_eq!(shown["issue_link"], LINK);
    let text = stdout(&sandbox.ok(&["instance", "show", &id]));
    assert!(text.contains(&format!("issue:     {LINK}")), "{text}");
    assert_eq!(
        stdout(&sandbox.ok(&["instance", "issue", &id])).trim(),
        LINK
    );
    let asked = sandbox.json(&["--json", "instance", "issue", &id]);
    assert_eq!(asked["issue_link"], LINK);

    let listed = sandbox.json(&["--json", "instances", "list"]);
    assert_eq!(listed[0]["issue_link"], LINK);

    let cleared = sandbox.json(&["--json", "instance", "issue", &id, "--clear"]);
    assert!(cleared["issue_link"].is_null());
    let shown = sandbox.json(&["--json", "instance", "show", &id]);
    assert!(shown["issue_link"].is_null());
    let listed = sandbox.json(&["--json", "instances", "list"]);
    assert!(listed[0]["issue_link"].is_null());
}

#[test]
fn a_refused_issue_link_is_reported_and_the_previous_link_is_kept() {
    let sandbox = Sandbox::new();
    let id = sandbox.create("Alpha");
    sandbox.ok(&["instance", "issue", &id, LINK]);

    for bad in [
        "file:///etc/passwd",
        "not a link",
        "https://user:hunter2@example.invalid/1",
    ] {
        let refused = sandbox.run(&["instance", "issue", &id, bad]);
        assert!(!refused.status.success(), "{bad}");
        let message = stderr(&refused);
        assert!(!message.is_empty(), "{bad}");
        assert!(!message.contains("hunter2"), "{message}");
    }

    let shown = sandbox.json(&["--json", "instance", "show", &id]);
    assert_eq!(shown["issue_link"], LINK);
}

#[test]
fn clearing_cannot_be_combined_with_a_new_link() {
    let sandbox = Sandbox::new();
    let id = sandbox.create("Alpha");
    assert!(
        !sandbox
            .run(&["instance", "issue", &id, LINK, "--clear"])
            .status
            .success()
    );
}

#[test]
fn a_duplicate_keeps_the_issue_link_of_its_original() {
    let sandbox = Sandbox::new();
    let id = sandbox.create("Alpha");
    sandbox.ok(&["instance", "issue", &id, LINK]);

    let copy = sandbox.json(&["--json", "instance", "duplicate", &id]);
    assert_eq!(copy["issue_link"], LINK);
    let copy_id = copy["id"].as_str().expect("id");
    let shown = sandbox.json(&["--json", "instance", "show", copy_id]);
    assert_eq!(shown["issue_link"], LINK);
}

#[test]
fn an_issue_link_is_refused_while_the_instance_files_are_missing() {
    let sandbox = Sandbox::new();
    let created = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Alpha",
        "--minecraft",
        "1.20.1",
        "--persistent",
    ]);
    let id = created["id"].as_str().expect("id").to_owned();
    sandbox.ok(&["instance", "issue", &id, LINK]);
    let root = std::path::PathBuf::from(created["root"].as_str().expect("root"));
    let backup = sandbox.base().join("unmounted");
    std::fs::rename(&root, &backup).expect("unmount");

    let refused = sandbox.run(&["instance", "issue", &id, "--clear"]);
    assert!(!refused.status.success());
    assert!(!stderr(&refused).is_empty());

    std::fs::rename(&backup, &root).expect("remount");
    let shown = sandbox.json(&["--json", "instance", "show", &id]);
    assert_eq!(shown["issue_link"], LINK);
}
