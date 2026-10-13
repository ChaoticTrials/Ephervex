//! Command-line coverage for the preferred mod catalog, offline.
//!
//! The setting decides which catalog a command reads when none is named, so
//! what a separate process proves is exactly what an in-process test cannot:
//! that the value one run records is the value the next run browses with, and
//! that recording one talks to no catalog at all.

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

    /// The settings file, which exists only once something has been recorded.
    fn config_file(&self) -> PathBuf {
        let output = self.run(&["config", "show"]);
        assert!(output.status.success(), "config show failed");
        let text = String::from_utf8_lossy(&output.stdout);
        let first = text.lines().next().expect("a header line");
        PathBuf::from(first.trim_start_matches("# ").trim())
    }

    fn instance(&self) -> String {
        let created = self.json(&[
            "--json",
            "instance",
            "create",
            "Catalog preference check",
            "--minecraft",
            "1.20.1",
            "--loader",
            "forge",
            "--loader-version",
            "47.4.10",
        ]);
        created["id"].as_str().expect("an id").to_owned()
    }
}

#[test]
fn a_first_run_reports_the_built_in_catalog_without_calling_it_a_choice() {
    let sandbox = Sandbox::new();
    let shown = sandbox.json(&["--json", "mod", "catalog"]);

    assert_eq!(shown["catalog"], "modrinth");
    assert_eq!(shown["configured"], "");
    assert_eq!(shown["chosen"], false);
    assert_eq!(shown["recognized"], true);
    assert!(
        !sandbox.config_file().exists(),
        "reading the setting must not create the settings file"
    );
}

#[test]
fn a_recorded_catalog_is_what_the_next_process_browses_with() {
    let sandbox = Sandbox::new();
    let recorded = sandbox.json(&["--json", "mod", "catalog", "curseforge"]);
    assert_eq!(recorded["catalog"], "curseforge");
    assert_eq!(recorded["chosen"], true);

    // A separate process, so nothing is being read out of memory.
    let reopened = sandbox.json(&["--json", "mod", "catalog"]);
    assert_eq!(reopened["catalog"], "curseforge");
    assert_eq!(reopened["configured"], "curseforge");
}

#[test]
fn recording_the_built_in_catalog_stays_distinct_from_recording_nothing() {
    let sandbox = Sandbox::new();
    let chosen = sandbox.json(&["--json", "mod", "catalog", "modrinth"]);
    assert_eq!(chosen["catalog"], "modrinth");
    assert_eq!(
        chosen["chosen"], true,
        "a catalog the user picked is a decision even when it matches the default"
    );

    let cleared = sandbox.json(&["--json", "mod", "catalog", "--clear"]);
    assert_eq!(cleared["catalog"], "modrinth");
    assert_eq!(cleared["chosen"], false);
}

#[test]
fn naming_curseforge_records_a_catalog_and_no_credential() {
    let sandbox = Sandbox::new();
    sandbox.json(&["--json", "mod", "catalog", "curseforge"]);

    let written = std::fs::read_to_string(sandbox.config_file()).expect("read settings");
    assert!(written.contains("default_catalog = \"curseforge\""));
    for marker in ["token", "secret", "api_key", "apikey", "credential"] {
        assert!(
            !written.to_ascii_lowercase().contains(marker),
            "preferring CurseForge must not put credential-shaped keys in the settings file"
        );
    }
}

#[test]
fn a_catalog_a_newer_ephervex_recorded_is_kept_and_never_searched_past() {
    let sandbox = Sandbox::new();
    let instance = sandbox.instance();
    sandbox.json(&["--json", "mod", "catalog", "curseforge"]);

    let path = sandbox.config_file();
    let written = std::fs::read_to_string(&path).expect("read settings");
    std::fs::write(
        &path,
        written.replace(
            "default_catalog = \"curseforge\"",
            "default_catalog = \"a-later-catalog\"",
        ),
    )
    .expect("write settings");

    let shown = sandbox.json(&["--json", "mod", "catalog"]);
    assert_eq!(shown["configured"], "a-later-catalog");
    assert_eq!(shown["recognized"], false);
    assert!(shown["catalog"].is_null(), "no catalog stands in for it");

    // Offline and immediate: the refusal happens before any catalog is asked
    // anything, rather than the search quietly moving to another one.
    let refused = sandbox.run(&["--json", "mod", "search", &instance, "jei"]);
    assert!(!refused.status.success());
    let reported = String::from_utf8_lossy(&refused.stderr);
    assert!(
        reported.contains("a-later-catalog"),
        "the refusal names the recorded value: {reported}"
    );

    let kept = std::fs::read_to_string(&path).expect("re-read settings");
    assert!(
        kept.contains("a-later-catalog"),
        "a build that cannot resolve the name must not overwrite it"
    );
}

#[test]
fn naming_a_catalog_on_the_command_line_never_consults_the_setting() {
    let sandbox = Sandbox::new();
    sandbox.json(&["--json", "mod", "catalog", "curseforge"]);
    let path = sandbox.config_file();
    let written = std::fs::read_to_string(&path).expect("read settings");
    std::fs::write(
        &path,
        written.replace(
            "default_catalog = \"curseforge\"",
            "default_catalog = \"a-later-catalog\"",
        ),
    )
    .expect("write settings");

    // An instance that does not exist, so the command fails without a catalog
    // being asked anything and this stays offline. What it distinguishes is
    // *which* refusal comes back: naming a catalog explicitly gets past the
    // preference entirely, while leaving it off does not.
    let missing = "00000000-0000-0000-0000-000000000000";
    let named = sandbox.run(&[
        "--json",
        "mod",
        "search",
        missing,
        "jei",
        "--provider",
        "modrinth",
    ]);
    assert!(!named.status.success());
    let reported = String::from_utf8_lossy(&named.stderr);
    assert!(
        !reported.contains("a-later-catalog"),
        "an explicitly named catalog never consults the preference: {reported}"
    );

    let unnamed = sandbox.run(&["--json", "mod", "search", missing, "jei"]);
    assert!(!unnamed.status.success());
    assert!(
        String::from_utf8_lossy(&unnamed.stderr).contains("a-later-catalog"),
        "without one, the unresolvable preference is what stops the command"
    );
}
