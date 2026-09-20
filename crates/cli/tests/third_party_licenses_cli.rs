//! `ephervex licenses` regression coverage, driven through the binary.
//!
//! The inventory is compiled into the binary, so every run here is pointed at a
//! throwaway directory through the XDG environment variables and `TMPDIR` to
//! prove exactly that: the command answers with no configuration, no database,
//! and no source tree (ADR 0003).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

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

    fn run(&self, args: &[&str]) -> Output {
        let base = self.base();
        Command::new(env!("CARGO_BIN_EXE_ephervex"))
            .args(args)
            // Deliberately run from the filesystem root: a listing that needed
            // the repository it was built in would be a listing a released
            // artifact cannot produce.
            .current_dir("/")
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

    fn succeeds(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "`ephervex {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("stdout is UTF-8")
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        serde_json::from_str(&self.succeeds(args)).expect("the output is JSON")
    }
}

#[test]
fn the_summary_counts_the_dependencies_and_their_licenses() {
    let sandbox = Sandbox::new();
    let output = sandbox.succeeds(&["licenses"]);
    assert!(output.contains("open-source packages"), "{output}");
    // Every release depends on crates under both of these, so a summary
    // missing either is reporting something other than this build.
    assert!(output.contains("Apache-2.0"), "{output}");
    assert!(output.contains("MIT"), "{output}");
    assert!(output.contains("THIRD-PARTY-LICENSES.md"), "{output}");
}

#[test]
fn every_dependency_is_listed_with_its_version_and_license() {
    let sandbox = Sandbox::new();
    let summary = sandbox.json(&["--json", "licenses"]);
    let listing = sandbox.succeeds(&["licenses", "--all"]);

    let dependencies = summary["dependencies"].as_array().expect("dependencies");
    assert!(!dependencies.is_empty());
    for dependency in dependencies {
        let name = dependency["name"].as_str().expect("a name");
        let version = dependency["version"].as_str().expect("a version");
        assert!(
            listing.contains(name) && listing.contains(version),
            "--all does not list {name} {version}"
        );
    }
}

#[test]
fn the_machine_readable_listing_carries_the_texts_as_well_as_the_names() {
    // A listing that named licenses without carrying them would send the
    // reader back to the file this command exists to replace.
    let sandbox = Sandbox::new();
    let inventory = sandbox.json(&["--json", "licenses"]);

    let licenses = inventory["licenses"].as_array().expect("licenses");
    assert!(!licenses.is_empty());
    for license in licenses {
        assert!(!license["id"].as_str().expect("an id").is_empty());
        assert!(!license["text"].as_str().expect("a text").is_empty());
    }

    for dependency in inventory["dependencies"].as_array().expect("dependencies") {
        let indices = dependency["texts"].as_array().expect("texts");
        assert!(
            !indices.is_empty(),
            "{} is listed with no license text",
            dependency["name"]
        );
        for index in indices {
            let index = usize::try_from(index.as_u64().expect("an index")).expect("in range");
            assert!(index < licenses.len(), "{index} names no license");
        }
    }
}

#[test]
fn a_license_can_be_printed_in_full_by_its_identifier() {
    let sandbox = Sandbox::new();
    let text = sandbox.succeeds(&["licenses", "--text", "Apache-2.0"]);
    assert!(
        text.contains("Apache License"),
        "{}",
        &text[..200.min(text.len())]
    );
    assert!(text.contains("TERMS AND CONDITIONS"));
}

#[test]
fn an_identifier_is_matched_whatever_its_case() {
    let sandbox = Sandbox::new();
    let lower = sandbox.succeeds(&["licenses", "--text", "apache-2.0"]);
    let exact = sandbox.succeeds(&["licenses", "--text", "Apache-2.0"]);
    assert_eq!(lower, exact);
}

#[test]
fn a_license_no_dependency_uses_is_refused_with_a_way_forward() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["licenses", "--text", "GPL-3.0-only"]);
    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("GPL-3.0-only"), "{message}");
    assert!(message.contains("ephervex licenses"), "{message}");
}

#[test]
fn the_slint_terms_the_desktop_application_is_built_under_are_reachable() {
    // Slint is offered under three licenses and Ephervex takes the
    // royalty-free one, whose attribution condition the About screen satisfies
    // (ADR 0001). A user has to be able to read the terms that actually apply.
    let sandbox = Sandbox::new();
    let text = sandbox.succeeds(&["licenses", "--text", "LicenseRef-Slint-Royalty-free-2.0"]);
    assert!(text.contains("Slint Royalty-free"), "{text}");
    assert!(text.contains("AboutSlint"), "{text}");
}

#[test]
fn nothing_is_written_outside_the_throwaway_roots() {
    // The command opens no storage at all, so a run that created a directory
    // would mean it reached something it has no reason to touch.
    let sandbox = Sandbox::new();
    sandbox.succeeds(&["licenses"]);
    let mut stray = Vec::new();
    for entry in std::fs::read_dir(sandbox.base()).expect("read the sandbox") {
        let name = entry.expect("an entry").file_name();
        let name = name.to_string_lossy().into_owned();
        if !["config", "data", "cache", "state", "tmp"].contains(&name.as_str()) {
            stray.push(name);
        }
    }
    assert!(stray.is_empty(), "the command wrote {stray:?}");
}
