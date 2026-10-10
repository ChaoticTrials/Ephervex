//! CurseForge pack import's offline command-line regression coverage.
//!
//! `--inspect` only ever reads the export's own manifest, so this drives it
//! against a synthetic archive that declares files by project and file id but
//! never contacts CurseForge: no API key is configured, and no loopback
//! server is started, which is itself part of what this file proves.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write as _;
use std::path::Path;
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

fn archive(path: &Path, manifest: &[u8], entries: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).expect("create archive");
    let mut writer = zip::ZipWriter::new(file);
    let options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    writer
        .start_file("manifest.json", options)
        .expect("start manifest");
    writer.write_all(manifest).expect("write manifest");
    for (name, contents) in entries {
        writer.start_file(*name, options).expect("start entry");
        writer.write_all(contents).expect("write entry");
    }
    writer.finish().expect("finish archive");
}

/// One required and one optional file declared by project and file id, per
/// the CurseForge export format: see `fixtures/curseforge/SOURCES.md` for why
/// these manifests are hand-authored inline rather than derived from a real
/// pack.
const MANIFEST: &[u8] = br#"{
    "minecraft": {
        "version": "1.20.1",
        "modLoaders": [{"id": "neoforge-20.4.190", "primary": true}]
    },
    "manifestType": "minecraftModpack",
    "manifestVersion": 1,
    "name": "Inspect Only Pack",
    "version": "2.0.0",
    "author": "A CLI Tester",
    "files": [
        {"projectID": 111111, "fileID": 222222, "required": true},
        {"projectID": 333333, "fileID": 444444, "required": false}
    ],
    "overrides": "overrides"
}"#;

#[test]
fn inspect_reports_a_curseforge_export_without_a_key_or_network_access() {
    let sandbox = Sandbox::new();
    let pack = sandbox.base().join("curseforge-export.zip");
    archive(
        &pack,
        MANIFEST,
        &[("overrides/config/example.toml", b"debug = true\n")],
    );

    let inspected = sandbox.json(&[
        "--json",
        "import",
        pack.to_str().expect("UTF-8 path"),
        "--inspect",
    ]);
    assert_eq!(inspected["format"], "CurseForge");
    assert_eq!(inspected["name"], "Inspect Only Pack");
    assert_eq!(inspected["version"], "2.0.0");
    assert_eq!(inspected["author"], "A CLI Tester");
    assert_eq!(inspected["minecraft_version"], "1.20.1");
    assert_eq!(inspected["loader"]["kind"], "NeoForge");
    assert_eq!(inspected["loader"]["version"], "20.4.190");
    assert_eq!(inspected["required_files"], 1);
    assert_eq!(inspected["optional_file_count"], 1);
    assert_eq!(
        inspected["needs_provider_resolution"], true,
        "the declared files are only project/file ids until CurseForge resolves them"
    );
    assert_eq!(
        inspected["optional_files"],
        serde_json::json!([]),
        "pending files have no logical path to select by until resolution"
    );

    let listed = sandbox.json(&["--json", "instances", "list"]);
    assert!(
        listed.as_array().expect("instances").is_empty(),
        "--inspect must not write an instance"
    );
}

#[test]
fn text_inspect_escapes_control_characters_in_the_manifest_author() {
    let sandbox = Sandbox::new();
    let pack = sandbox.base().join("spoofed-author.zip");
    let manifest = String::from_utf8(MANIFEST.to_vec())
        .expect("UTF-8 manifest")
        .replace("A CLI Tester", "A\\u001b[2J\\nsource:    Trusted");
    archive(&pack, manifest.as_bytes(), &[]);
    let path = pack.to_str().expect("UTF-8 path");

    let inspected = sandbox.run(&["import", path, "--inspect"]);
    assert!(inspected.status.success(), "{inspected:?}");
    let text = String::from_utf8(inspected.stdout).expect("UTF-8 output");
    assert!(
        text.contains("author:    A\\u{1b}[2J\\u{a}source:    Trusted\n"),
        "{text:?}"
    );
    assert!(
        text.chars().all(|c| c == '\n' || !c.is_control()),
        "raw control characters reached the terminal: {text:?}"
    );

    let machine = sandbox.json(&["--json", "import", path, "--inspect"]);
    assert_eq!(machine["author"], "A\u{1b}[2J\nsource:    Trusted");
}
