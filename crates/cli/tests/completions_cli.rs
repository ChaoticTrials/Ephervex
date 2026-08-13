//! Shell completion generation through the public `ephervex` binary.

#![allow(clippy::expect_used, clippy::panic)]

use std::process::{Command, Output};

const SUPPORTED_SHELLS: &[&str] = &["bash", "elvish", "fish", "powershell", "zsh"];

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ephervex"))
        .args(args)
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .env("EPHERVEX_LOG", "error")
        .output()
        .expect("run the ephervex binary")
}

#[test]
fn every_supported_shell_generates_a_non_empty_script_offline() {
    for shell in SUPPORTED_SHELLS {
        let output = run(&["completions", shell]);
        assert!(
            output.status.success(),
            "completion generation failed for {shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !output.stdout.is_empty(),
            "completion generation produced no output for {shell}"
        );
        assert!(
            output.stderr.is_empty(),
            "completion generation wrote to stderr for {shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn generated_scripts_include_current_commands_and_options() {
    for shell in SUPPORTED_SHELLS {
        let output = run(&["completions", shell]);
        assert!(output.status.success());
        let script = String::from_utf8_lossy(&output.stdout);

        for expected in [
            "instance",
            "server",
            "logs",
            "completions",
            "duplicate",
            "properties",
            "loader-version",
            "keep-files",
        ] {
            assert!(
                script.contains(expected),
                "{shell} completions do not include {expected:?}"
            );
        }
    }
}

#[test]
fn an_invalid_shell_is_rejected_by_argument_parsing() {
    let output = run(&["completions", "not-a-shell"]);

    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("invalid value 'not-a-shell'"), "{error}");
    assert!(error.contains("possible values"), "{error}");
}
