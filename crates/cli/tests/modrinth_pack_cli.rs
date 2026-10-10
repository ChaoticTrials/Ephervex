//! Modrinth pack import's offline command-line regression coverage.
//!
//! The archive used for the real import deliberately has no downloadable
//! files. That keeps ordinary CI independent of Modrinth while still driving
//! analysis, override layering, retained-source storage, and server recipe
//! reconstruction through separate `ephervex` processes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

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
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "`ephervex {}` did not print JSON ({error}):\n{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }
}

fn archive(path: &Path, index: &[u8], entries: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).expect("create archive");
    let mut writer = zip::ZipWriter::new(file);
    let options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    writer
        .start_file("modrinth.index.json", options)
        .expect("start index");
    writer.write_all(index).expect("write index");
    for (name, contents) in entries {
        writer.start_file(*name, options).expect("start entry");
        writer.write_all(contents).expect("write entry");
    }
    writer.finish().expect("finish archive");
}

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/modrinth")
        .join(relative)
}

#[test]
fn inspect_reports_the_workflow_pack_without_writing_or_downloading() {
    let sandbox = Sandbox::new();
    let pack = sandbox.base().join("layered-client-server.mrpack");
    archive(
        &pack,
        &std::fs::read(fixture("layered-client-server/modrinth.index.json"))
            .expect("fixture index"),
        &[],
    );

    let inspected = sandbox.json(&[
        "--json",
        "import",
        pack.to_str().expect("UTF-8 path"),
        "--inspect",
    ]);
    assert_eq!(inspected["format"], "Modrinth");
    assert_eq!(inspected["minecraft_version"], "1.20.1");
    assert_eq!(inspected["loader"]["kind"], "NeoForge");
    assert_eq!(inspected["required_files"], 2);
    assert_eq!(inspected["optional_files"][0]["selected"], false);

    let listed = sandbox.json(&["--json", "instances", "list"]);
    assert!(listed.as_array().expect("instances").is_empty());
}

#[test]
fn an_overrides_only_import_retains_a_server_recipe_after_the_archive_is_deleted() {
    let sandbox = Sandbox::new();
    let pack = sandbox.base().join("overrides-only.mrpack");
    let index = br#"{
        "formatVersion": 1,
        "game": "minecraft",
        "versionId": "offline-1",
        "name": "Offline Overrides",
        "files": [],
        "dependencies": { "minecraft": "1.20.1" }
    }"#;
    archive(
        &pack,
        index,
        &[
            ("overrides/options.txt", b"guiScale:1\n"),
            ("overrides/config/common.toml", b"common=true\n"),
            ("client-overrides/options.txt", b"guiScale:3\n"),
            ("server-overrides/server.properties", b"view-distance=12\n"),
        ],
    );

    // Managed preferences are last in the required ordering, after both override
    // layers. This also proves the import took place without a download.
    sandbox.json(&["--json", "preferences", "set", "gui_scale", "2"]);
    let imported = sandbox.json(&[
        "--json",
        "import",
        pack.to_str().expect("UTF-8 path"),
        "--persistent",
    ]);
    assert_eq!(imported["pack"]["name"], "Offline Overrides");
    assert_eq!(imported["installed"], serde_json::json!([]));

    let root = PathBuf::from(imported["root"].as_str().expect("instance root"));
    let options = std::fs::read_to_string(root.join("minecraft/options.txt")).expect("options");
    assert!(options.contains("guiScale:2"), "{options}");
    assert!(!options.contains("guiScale:3"), "{options}");
    assert!(root.join("minecraft/config/common.toml").is_file());

    let id = imported["id"].as_str().expect("instance id");
    std::fs::remove_file(&pack).expect("delete original archive");
    let server = sandbox.json(&["--json", "instance", "recipe", id, "--side", "server"]);
    assert_eq!(server["side"], "server");
    assert_eq!(server["minecraft_version"], "1.20.1");
    assert_eq!(server["files"], serde_json::json!([]));
    assert_eq!(server["layers"][0]["scope"], "common");
    assert_eq!(server["layers"][1]["scope"], "server");
    assert_eq!(server["layers_retained"], true);
}

#[test]
fn text_output_escapes_control_characters_in_archive_supplied_pack_text() {
    let sandbox = Sandbox::new();
    let pack = sandbox.base().join("spoofed.mrpack");
    let index = br#"{
        "formatVersion": 1,
        "game": "minecraft",
        "versionId": "1.0\u001b[31m",
        "name": "Spoof\u001b[2J\nsource:    Trusted",
        "files": [],
        "dependencies": { "minecraft": "1.20.1" }
    }"#;
    archive(&pack, index, &[]);
    let path = pack.to_str().expect("UTF-8 path");

    let inspected = sandbox.run(&["import", path, "--inspect"]);
    assert!(inspected.status.success(), "{inspected:?}");
    let text = String::from_utf8(inspected.stdout).expect("UTF-8 output");
    assert!(
        text.contains("pack:      Spoof\\u{1b}[2J\\u{a}source:    Trusted\n"),
        "{text:?}"
    );
    assert!(text.contains("version:   1.0\\u{1b}[31m\n"), "{text:?}");
    assert!(
        text.chars().all(|c| c == '\n' || !c.is_control()),
        "raw control characters reached the terminal: {text:?}"
    );
    assert_eq!(text.matches("source:").count(), 2, "{text:?}");

    let machine = sandbox.json(&["--json", "import", path, "--inspect"]);
    assert_eq!(machine["name"], "Spoof\u{1b}[2J\nsource:    Trusted");
    assert_eq!(machine["version"], "1.0\u{1b}[31m");

    let imported = sandbox.run(&["import", path, "--persistent"]);
    assert!(imported.status.success(), "{imported:?}");
    let summary = String::from_utf8(imported.stdout).expect("UTF-8 output");
    assert!(
        summary.chars().all(|c| c == '\n' || !c.is_control()),
        "raw control characters reached the terminal: {summary:?}"
    );
    assert!(
        summary.contains("pack:      Spoof\\u{1b}[2J"),
        "{summary:?}"
    );
}

#[test]
fn text_output_escapes_control_characters_in_refused_download_addresses() {
    let sandbox = Sandbox::new();
    let pack = sandbox.base().join("refused-address.mrpack");
    let index = br#"{
        "formatVersion": 1,
        "game": "minecraft",
        "versionId": "1.0",
        "name": "Refused Address",
        "files": [{
            "path": "mods/a.jar",
            "hashes": {
                "sha1": "1726b9210f6a517b654e0fec81091468d30b688f",
                "sha512": "e348a4a3405b631429af878201d448d9b50eeb28cbb07143d5c52ec587b35a30e7a8d6e3e89a0f3c9c4d647622957d358c5d9c6b7403796552a307bc7c0a7f59"
            },
            "downloads": [
                "https://cdn.modrinth.com/data/x/a.jar",
                "\u001b[2Jevil"
            ],
            "fileSize": 201
        }],
        "dependencies": { "minecraft": "1.20.1" }
    }"#;
    archive(&pack, index, &[]);

    let inspected = sandbox.run(&["import", pack.to_str().expect("UTF-8 path"), "--inspect"]);
    assert!(inspected.status.success(), "{inspected:?}");
    let stderr = String::from_utf8(inspected.stderr).expect("UTF-8 output");
    assert!(stderr.contains("  \\u{1b}[2Jevil\n"), "{stderr:?}");
    assert!(
        stderr.chars().all(|c| c == '\n' || !c.is_control()),
        "raw control characters reached the terminal: {stderr:?}"
    );
}

#[test]
fn a_fabric_pack_fails_with_a_machine_readable_message_id() {
    let sandbox = Sandbox::new();
    let pack = sandbox.base().join("fabric.mrpack");
    archive(
        &pack,
        &std::fs::read(fixture("invalid/fabric.index.json")).expect("fixture index"),
        &[],
    );

    let output = sandbox.run(&[
        "--json",
        "import",
        pack.to_str().expect("UTF-8 path"),
        "--inspect",
    ]);
    assert!(!output.status.success(), "Fabric must not be imported");
    let failure: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("machine-readable error");
    assert_eq!(failure["message_id"], "error-pack-loader-unsupported");
    assert!(
        failure["error"]
            .as_str()
            .expect("localized error")
            .contains("fabric-loader")
    );
    assert!(
        sandbox
            .json(&["--json", "instances", "list"])
            .as_array()
            .expect("instances")
            .is_empty()
    );
}

#[test]
fn import_errors_escape_control_characters_in_archive_paths_but_json_stays_exact() {
    let sandbox = Sandbox::new();
    let cases = [
        // Rejected while validating the path itself.
        ("unsafe-path", "../\u{1b}[2Jx.jar", "{\"sha1\":\"\"}"),
        // A well-formed path whose entry is refused for another reason.
        ("missing-hashes", "mods/\u{1b}[2Jx.jar", "{}"),
    ];
    for (name, entry, hashes) in cases {
        let pack = sandbox.base().join(format!("{name}.mrpack"));
        let index = serde_json::json!({
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": "1.0",
            "name": "Hostile Entry",
            "files": [{
                "path": entry,
                "hashes": serde_json::from_str::<serde_json::Value>(hashes).unwrap(),
                "downloads": ["https://cdn.modrinth.com/data/x/a.jar"],
                "fileSize": 1
            }],
            "dependencies": { "minecraft": "1.20.1" }
        });
        archive(&pack, index.to_string().as_bytes(), &[]);
        let path = pack.to_str().expect("UTF-8 path");

        let failed = sandbox.run(&["import", path, "--inspect"]);
        assert!(!failed.status.success(), "{name}: {failed:?}");
        let stderr = String::from_utf8(failed.stderr).expect("UTF-8 output");
        assert!(
            stderr.chars().all(|c| c == '\n' || !c.is_control()),
            "{name}: raw control characters reached the terminal: {stderr:?}"
        );
        assert!(stderr.contains("\\u{1b}[2Jx.jar"), "{name}: {stderr:?}");

        let machine = sandbox.run(&["--json", "import", path, "--inspect"]);
        assert!(!machine.status.success(), "{name}: {machine:?}");
        let error: serde_json::Value =
            serde_json::from_slice(&machine.stderr).expect("JSON error output");
        assert!(
            error["detail"].to_string().contains("\\u001b[2Jx.jar"),
            "{name}: {error}"
        );
    }
}
