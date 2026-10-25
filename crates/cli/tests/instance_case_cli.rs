//! Writing down a case for an instance through the `ephervex` binary: the
//! steps to reproduce a bug, a note, and how often it showed up again.
//!
//! Every run is pointed at a throwaway directory through the XDG variables and
//! `TMPDIR`, and nothing here needs the network.
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

#[test]
fn an_instance_starts_with_an_empty_case() {
    let sandbox = Sandbox::new();
    let id = sandbox.create("Alpha");

    let case = sandbox.json(&["--json", "instance", "case", &id]);
    assert_eq!(case["steps"].as_array().expect("steps").len(), 0);
    assert_eq!(case["note"], "");
    assert_eq!(case["reproduced"], 0);
    let text = stdout(&sandbox.ok(&["instance", "case", &id]));
    assert!(text.contains("steps:      none"), "{text}");
    assert!(text.contains("reproduced: 0"), "{text}");
}

#[test]
fn steps_are_added_finished_and_removed_by_their_number() {
    let sandbox = Sandbox::new();
    let id = sandbox.create("Alpha");

    sandbox.ok(&["instance", "case", &id, "add-step", "Place a belt"]);
    let added = sandbox.json(&["--json", "instance", "case", &id, "add-step", "Fill it"]);
    let steps = added["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 2);
    let first = steps[0]["id"].as_u64().expect("id").to_string();
    let second = steps[1]["id"].as_u64().expect("id").to_string();
    assert_eq!(steps[0]["text"], "Place a belt");
    assert_eq!(steps[0]["done"], false);

    let done = sandbox.json(&["--json", "instance", "case", &id, "done", &first]);
    assert_eq!(done["steps"][0]["done"], true);
    assert_eq!(done["steps"][1]["done"], false);
    let text = stdout(&sandbox.ok(&["instance", "case", &id]));
    assert!(
        text.contains(&format!("{first}. [x] Place a belt")),
        "{text}"
    );
    assert!(text.contains(&format!("{second}. [ ] Fill it")), "{text}");

    let undone = sandbox.json(&["--json", "instance", "case", &id, "undo", &first]);
    assert_eq!(undone["steps"][0]["done"], false);

    let removed = sandbox.json(&["--json", "instance", "case", &id, "remove-step", &first]);
    let remaining = removed["steps"].as_array().expect("steps");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0]["text"], "Fill it");
}

#[test]
fn a_refused_step_is_reported_and_nothing_is_added() {
    let sandbox = Sandbox::new();
    let id = sandbox.create("Alpha");

    let empty = sandbox.run(&["instance", "case", &id, "add-step", ""]);
    assert!(!empty.status.success());
    assert!(!String::from_utf8_lossy(&empty.stderr).is_empty());
    let too_long = "x".repeat(301);
    assert!(
        !sandbox
            .run(&["instance", "case", &id, "add-step", &too_long])
            .status
            .success()
    );

    let case = sandbox.json(&["--json", "instance", "case", &id]);
    assert_eq!(case["steps"].as_array().expect("steps").len(), 0);
}

#[test]
fn the_note_is_replaced_and_cleared() {
    let sandbox = Sandbox::new();
    let id = sandbox.create("Alpha");

    let set = sandbox.json(&["--json", "instance", "case", &id, "note", "Only when full"]);
    assert_eq!(set["note"], "Only when full");
    assert!(stdout(&sandbox.ok(&["instance", "case", &id])).contains("note:       Only when full"));

    let cleared = sandbox.json(&["--json", "instance", "case", &id, "note", "--clear"]);
    assert_eq!(cleared["note"], "");
    assert!(
        !sandbox
            .run(&["instance", "case", &id, "note", "text", "--clear"])
            .status
            .success(),
        "a note cannot be set and cleared at once"
    );
    assert!(
        !sandbox
            .run(&["instance", "case", &id, "note"])
            .status
            .success(),
        "a note needs its text or --clear"
    );
}

#[test]
fn the_reproduced_count_goes_up_and_starts_over() {
    let sandbox = Sandbox::new();
    let id = sandbox.create("Alpha");

    sandbox.ok(&["instance", "case", &id, "reproduced"]);
    let counted = sandbox.json(&["--json", "instance", "case", &id, "reproduced"]);
    assert_eq!(counted["reproduced"], 2);

    let reset = sandbox.json(&["--json", "instance", "case", &id, "reproduced", "--reset"]);
    assert_eq!(reset["reproduced"], 0);
}

#[test]
fn a_case_command_for_an_unknown_instance_is_refused() {
    let sandbox = Sandbox::new();
    let refused = sandbox.run(&[
        "instance",
        "case",
        "00000000-0000-0000-0000-000000000000",
        "add-step",
        "Place a belt",
    ]);
    assert!(!refused.status.success());
}
