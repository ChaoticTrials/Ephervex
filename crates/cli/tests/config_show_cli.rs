//! Command-line coverage for `config show`: the JSON form carries the same
//! configuration as the text form.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
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

    fn write_config(&self, text: &str) {
        let directory = self.base().join("config").join("ephervex");
        std::fs::create_dir_all(&directory).expect("config dir");
        std::fs::write(directory.join("config.toml"), text).expect("write config");
    }

    fn run(&self, args: &[&str]) -> Output {
        let base = self.base();
        Command::new(env!("CARGO_BIN_EXE_ephervex"))
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
        serde_json::from_slice(&output.stdout).expect("JSON on stdout")
    }

    fn text(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "`ephervex {}` failed",
            args.join(" ")
        );
        String::from_utf8(output.stdout).expect("UTF-8 output")
    }
}

#[test]
fn json_output_carries_the_sections_the_text_output_prints() {
    let sandbox = Sandbox::new();
    sandbox.write_config(
        "schema_version = 1\nlanguage = \"de-DE\"\n\n[cache]\nartifact_limit_bytes = 12345\n",
    );

    let text = sandbox.text(&["config", "show"]);
    let shown = sandbox.json(&["--json", "config", "show"]);

    let config = shown["config"].as_object().expect("a config object");
    assert_eq!(config["language"], "de-DE");
    for section in [
        "java",
        "display",
        "mods",
        "import",
        "server_defaults",
        "eula",
        "api",
        "cache",
    ] {
        assert!(
            config.contains_key(section),
            "the JSON form leaves out [{section}]"
        );
        assert!(
            text.contains(&format!("[{section}]")) || text.contains(&format!("{section}.")),
            "the text form no longer prints [{section}], so this comparison is stale"
        );
    }
    assert_eq!(config["cache"]["artifact_limit_bytes"], 12345);
    assert_eq!(shown["exists"], true);
}

#[test]
fn json_output_reports_a_first_run_configuration_with_its_defaults() {
    let sandbox = Sandbox::new();
    let shown = sandbox.json(&["--json", "config", "show"]);

    assert_eq!(shown["exists"], false);
    assert_eq!(shown["config"]["language"], "en-US");
    assert!(shown["config"]["api"].is_object());
}

#[test]
fn a_preserved_unknown_date_is_shown_as_text_in_json() {
    let sandbox = Sandbox::new();
    sandbox.write_config("schema_version = 1\nfuture_setting = 1979-05-27T07:32:00Z\n");

    let shown = sandbox.json(&["--json", "config", "show"]);

    assert_eq!(shown["config"]["future_setting"], "1979-05-27T07:32:00Z");
}
