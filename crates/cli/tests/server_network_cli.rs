//! Argument handling of `ephervex server network`, driven through the `ephervex` binary.
//!
//! Every run is pointed at a throwaway directory through the XDG variables and
//! `TMPDIR`, set on a child process, so nothing here touches real servers and
//! nothing here needs the network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Output;

use tempfile::TempDir;

mod binary_sandbox;

const SERVER: &str = "00000000-0000-4000-8000-000000000001";

fn run(sandbox: &TempDir, args: &[&str]) -> Output {
    let base = sandbox.path();
    for child in ["config", "data", "cache", "state", "tmp"] {
        std::fs::create_dir_all(base.join(child)).expect("create root");
    }
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

#[test]
fn network_command_without_any_option_is_rejected_instead_of_resetting_settings() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let output = run(&sandbox, &["server", "network", SERVER]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--online-mode"), "{stderr}");
    assert!(stderr.contains("--port"), "{stderr}");
}

#[test]
fn online_mode_accepts_an_explicit_boolean_and_a_bare_flag() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    for arguments in [
        ["server", "network", SERVER, "--online-mode=false"],
        ["server", "network", SERVER, "--online-mode=true"],
        ["server", "network", SERVER, "--online-mode"],
    ] {
        let output = run(&sandbox, &arguments);
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Parsing succeeds; the unknown server is what the command then refuses.
        assert!(!stderr.contains("Usage:"), "{arguments:?}: {stderr}");
        assert!(!output.status.success(), "{arguments:?}");
    }
}

#[test]
fn network_help_documents_that_unspecified_settings_are_kept() {
    let sandbox = tempfile::tempdir().expect("temp dir");
    let output = run(&sandbox, &["server", "network", "--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--online-mode=false"), "{stdout}");
    assert!(stdout.contains("stored address is kept"), "{stdout}");
}
