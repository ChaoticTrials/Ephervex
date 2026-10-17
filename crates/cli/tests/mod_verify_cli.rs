//! What `mod verify` says about an instance's files and its shared files.
//!
//! The check is only worth running if its output can be believed: a file with
//! no recorded digest is not proven intact, a shared file that has not been
//! downloaded was not checked, and `--json` is one document a script can parse.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Output;

use tempfile::TempDir;

mod binary_sandbox;
mod support;

/// The SHA-1 of zero bytes, so an empty file matches it without a hashing crate.
const EMPTY_SHA1: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";

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

    fn run(&self, args: &[&str]) -> Output {
        let base = self.directory.path();
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

    /// Creates a vanilla instance holding one jar the recipe records a digest
    /// for, which matches, and one it records none for.
    fn instance_with_an_unproven_jar(&self) -> String {
        self.instance_with_an_unproven_jar_at().0
    }

    /// [`Self::instance_with_an_unproven_jar`], with the instance's root.
    fn instance_with_an_unproven_jar_at(&self) -> (String, PathBuf) {
        let created = self.run(&[
            "--json",
            "instance",
            "create",
            "Verify Example",
            "--minecraft",
            "1.20.1",
        ]);
        assert!(
            created.status.success(),
            "create: {}",
            String::from_utf8_lossy(&created.stderr)
        );
        let value: serde_json::Value =
            serde_json::from_slice(&created.stdout).expect("instance json");
        let id = value["id"].as_str().expect("an id").to_owned();
        let root = Path::new(value["root"].as_str().expect("a root")).to_owned();

        let recipe = serde_json::json!({
            "recipe_version": 1,
            "source_kind": "manual",
            "original_name": "Verify Example",
            "minecraft_version": "1.20.1",
            "loader": {"kind": "vanilla"},
            "files": [
                entry("proven.jar", &[("sha1", EMPTY_SHA1)]),
                entry("shipped.jar", &[]),
            ],
            "override_layers": [],
            "provider_metadata": {"kind": "manual"},
        });
        let source = root.join(".launcher").join("source");
        std::fs::create_dir_all(&source).expect("source dir");
        std::fs::write(
            source.join("recipe.json"),
            serde_json::to_vec(&recipe).expect("serialize"),
        )
        .expect("write recipe");

        let mods = root.join("minecraft").join("mods");
        std::fs::create_dir_all(&mods).expect("mods dir");
        std::fs::write(mods.join("proven.jar"), b"").expect("write jar");
        std::fs::write(mods.join("shipped.jar"), b"jar").expect("write jar");
        (id, root)
    }
}

fn entry(name: &str, hashes: &[(&str, &str)]) -> serde_json::Value {
    serde_json::json!({
        "logical_path": format!("mods/{name}"),
        "hashes": hashes
            .iter()
            .map(|(algorithm, digest)| serde_json::json!({"algorithm": algorithm, "digest": digest}))
            .collect::<Vec<_>>(),
        "downloads": [],
        "size": 0,
        "client": "required",
        "server": "required",
        "origin": "user_selected",
    })
}

#[test]
fn a_file_with_no_recorded_digest_is_named_and_not_called_intact() {
    let sandbox = Sandbox::new();
    let id = sandbox.instance_with_an_unproven_jar();

    let output = sandbox.run(&["mod", "verify", "--instance", &id]);
    assert!(
        output.status.success(),
        "an unproven file is not a fault: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("unproven  mods/shipped.jar"),
        "the unproven file is not named in: {stdout}"
    );
    assert!(
        !stdout.contains("all intact"),
        "unproven files were counted as intact: {stdout}"
    );
    assert!(
        stdout.contains("1 files intact, 1 unproven"),
        "the summary does not separate them: {stdout}"
    );
}

#[test]
fn a_put_aside_file_is_named_and_not_called_intact() {
    let sandbox = Sandbox::new();
    let (id, root) = sandbox.instance_with_an_unproven_jar_at();
    // What the mod list leaves when it puts a pack's jar aside.
    let kept = root.join(".launcher/put-aside/mods");
    std::fs::create_dir_all(&kept).expect("put-aside dir");
    std::fs::rename(
        root.join("minecraft/mods/proven.jar"),
        kept.join("proven.jar"),
    )
    .expect("put aside");

    let output = sandbox.run(&["mod", "verify", "--instance", &id]);
    assert!(
        output.status.success(),
        "a put-aside file is not a fault: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("put-aside  mods/proven.jar"),
        "the put-aside file is not named in: {stdout}"
    );
    assert!(
        stdout.contains(
            "0 files intact, 1 unproven: no digest is recorded to check them against, 1 put aside"
        ),
        "the summary does not separate them: {stdout}"
    );
}

#[test]
fn shared_files_not_downloaded_yet_are_not_counted_as_checked() {
    let sandbox = Sandbox::new();
    let id = sandbox.instance_with_an_unproven_jar();

    let output = sandbox.run(&["mod", "verify", "--instance", &id, "--shared"]);
    assert!(
        output.status.success(),
        "verify: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("are not downloaded yet"),
        "a fresh instance has nothing downloaded: {stdout}"
    );
    assert!(
        !stdout.contains("shared files checked"),
        "files that were never fetched were reported as checked: {stdout}"
    );
}

#[test]
fn shared_json_output_is_one_document_holding_both_reports() {
    let sandbox = Sandbox::new();
    let id = sandbox.instance_with_an_unproven_jar();

    let output = sandbox.run(&["--json", "mod", "verify", "--instance", &id, "--shared"]);
    assert!(
        output.status.success(),
        "verify: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is exactly one JSON document");
    assert_eq!(value["sound"], true);
    let files = value["files"].as_array().expect("the instance's files");
    assert_eq!(files.len(), 2);
    assert!(
        files
            .iter()
            .any(|file| file["path"] == "mods/shipped.jar" && file["integrity"] == "unverifiable")
    );
    assert_eq!(value["shared"]["metadata_cached"], true);
    assert!(value["shared"]["files"].is_array());
}

#[test]
fn json_output_without_shared_has_no_shared_report() {
    let sandbox = Sandbox::new();
    let id = sandbox.instance_with_an_unproven_jar();

    let output = sandbox.run(&["--json", "mod", "verify", "--instance", &id]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("one document");
    assert!(value.get("shared").is_none());
}

/// The digest the manifest fixture publishes for the 1.20.1 version document.
const V1_20_1_SHA1: &str = "8789eb9a3eb37911774a2e5552377d3460a57593";

fn find_file(directory: &Path, name: &str) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(directory).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|file| file == name) {
            return Some(path);
        }
    }
    None
}

#[test]
fn shared_verification_refuses_a_cached_version_document_that_no_longer_matches_its_digest() {
    let sandbox = Sandbox::new();
    let id = sandbox.instance_with_an_unproven_jar();
    let cached = find_file(
        &sandbox.directory.path().join("cache"),
        &format!("{V1_20_1_SHA1}.json"),
    )
    .expect("the seeded version document");
    let original = std::fs::read_to_string(&cached).expect("cached document");
    assert!(original.contains("net.minecraft.client.main.Main"));
    // Still valid metadata under the same file name; only its digest differs.
    std::fs::write(
        &cached,
        original.replace("net.minecraft.client.main.Main", "evil.Main"),
    )
    .expect("tamper with the cached document");

    let output = sandbox.run(&["mod", "verify", "--instance", &id, "--shared"]);

    assert!(
        !output.status.success(),
        "an altered document was trusted: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("shared files checked"),
        "integrity was reported from altered metadata: {stdout}"
    );
}

#[test]
fn shared_with_a_server_is_a_usage_error() {
    let sandbox = Sandbox::new();

    let output = sandbox.run(&[
        "mod",
        "verify",
        "--server",
        "00000000-0000-4000-8000-000000000002",
        "--shared",
    ]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "a server has no shared files to check, so the flag is refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--shared") && stderr.contains("--server"),
        "the error names both flags: {stderr}"
    );
    assert!(
        !stderr.contains("No such server"),
        "the server was looked up despite the refused flag: {stderr}"
    );
}
