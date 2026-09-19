//! Turning a mod off, and on, when other mods depend on it.
//!
//! A command has no way to ask, so it refuses instead. That is the whole point
//! of this file: the refusal has to change nothing and has to say so with its
//! exit status, or a script walks straight into a mod set that does not load.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

mod support;

/// The instance every test here builds: a library and three mods that need it,
/// one of which also declares an optional relation that must never cascade.
const LIBRARY: &str = "qEH6GYul";
const BUILDER: &str = "por2AZc5";
const ADDON: &str = "3Gdi7Qgq";
const UTILITY: &str = "MMMWYsjm";

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

    /// Creates the instance, writes its recipe and lays down its jars.
    fn instance_with_a_shared_library(&self) -> (String, PathBuf) {
        let created = self.run(&[
            "--json",
            "instance",
            "create",
            "Dependency Aware Example",
            "--minecraft",
            "1.20.1",
            "--loader",
            "forge",
            "--loader-version",
            "47.4.10",
        ]);
        assert!(
            created.status.success(),
            "create: {}",
            String::from_utf8_lossy(&created.stderr)
        );
        let value: serde_json::Value =
            serde_json::from_slice(&created.stdout).expect("instance json");
        let id = value["id"].as_str().expect("an id").to_owned();
        let root = PathBuf::from(value["root"].as_str().expect("a root"));

        let recipe = serde_json::json!({
            "recipe_version": 1,
            "source_kind": "manual",
            "original_name": "Dependency Aware Example",
            "minecraft_version": "1.20.1",
            "loader": {"kind": "forge", "version": "47.4.10"},
            "files": [
                entry("LibX.jar", LIBRARY, &[]),
                entry("SkyblockBuilder.jar", BUILDER, &[(LIBRARY, "required"), (ADDON, "optional")]),
                entry("SkyGUIs.jar", ADDON, &[(LIBRARY, "required"), (BUILDER, "required")]),
                entry("UtilitiX.jar", UTILITY, &[(LIBRARY, "required")]),
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
        for jar in [
            "LibX.jar",
            "SkyblockBuilder.jar",
            "SkyGUIs.jar",
            "UtilitiX.jar",
        ] {
            std::fs::write(mods.join(jar), b"jar").expect("write jar");
        }
        (id, mods)
    }
}

fn entry(name: &str, project: &str, relations: &[(&str, &str)]) -> serde_json::Value {
    serde_json::json!({
        "logical_path": format!("mods/{name}"),
        "hashes": [{"algorithm": "sha1", "digest": "da39a3ee5e6b4b0d3255bfef95601890afd80709"}],
        "downloads": [],
        "size": 3,
        "client": "required",
        "server": "required",
        "origin": "user_selected",
        "identity": {"provider": "modrinth", "project_id": project, "version_id": format!("{project}V1")},
        "relations": relations
            .iter()
            .map(|(target, kind)| serde_json::json!({
                "kind": kind,
                "target": {"project": {"project_id": target}},
            }))
            .collect::<Vec<_>>(),
    })
}

fn jars(mods: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(mods)
        .expect("read mods")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn disabling_a_library_refuses_and_changes_nothing() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    let before = jars(&mods);

    let output = sandbox.run(&["mod", "disable", "--instance", &id, "mods/LibX.jar"]);
    assert!(
        !output.status.success(),
        "a refusal has to fail the command"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    for expected in ["SkyGUIs.jar", "SkyblockBuilder.jar", "UtilitiX.jar"] {
        assert!(
            stderr.contains(expected),
            "{expected} is not named in: {stderr}"
        );
    }
    assert_eq!(jars(&mods), before, "nothing on disk may change");
}

#[test]
fn the_refusal_names_the_dependents_as_json() {
    let sandbox = Sandbox::new();
    let (id, _mods) = sandbox.instance_with_a_shared_library();

    let output = sandbox.run(&[
        "--json",
        "mod",
        "disable",
        "--instance",
        &id,
        "mods/LibX.jar",
    ]);
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("impact json");
    let breaks: Vec<&str> = value["breaks"]
        .as_array()
        .expect("breaks")
        .iter()
        .map(|entry| entry["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(breaks.len(), 3);
    assert!(value["complete"].as_bool().expect("complete"));
    assert!(
        value["degrades"].as_array().expect("degrades").is_empty(),
        "nothing declares the library optional"
    );
}

#[test]
fn with_dependents_turns_the_whole_set_off_library_last() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();

    let output = sandbox.run(&[
        "--json",
        "mod",
        "disable",
        "--instance",
        &id,
        "--with-dependents",
        "mods/LibX.jar",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let applied: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    let order: Vec<&str> = applied
        .as_array()
        .expect("an array")
        .iter()
        .map(|row| row["path"].as_str().expect("a path"))
        .collect();
    assert_eq!(
        order.last(),
        Some(&"mods/LibX.jar"),
        "what everything needs goes last, so no prefix leaves a set that cannot load"
    );
    assert_eq!(
        jars(&mods),
        [
            "LibX.jar.disabled",
            "SkyGUIs.jar.disabled",
            "SkyblockBuilder.jar.disabled",
            "UtilitiX.jar.disabled",
        ]
    );
}

#[test]
fn only_this_turns_off_the_named_mod_alone() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();

    let output = sandbox.run(&[
        "mod",
        "disable",
        "--instance",
        &id,
        "--only-this",
        "mods/LibX.jar",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        jars(&mods),
        [
            "LibX.jar.disabled",
            "SkyGUIs.jar",
            "SkyblockBuilder.jar",
            "UtilitiX.jar",
        ],
        "the user asked for exactly this, and got exactly this"
    );
}

#[test]
fn enabling_an_addon_brings_back_what_it_needs_and_nothing_else() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    assert!(
        sandbox
            .run(&[
                "mod",
                "disable",
                "--instance",
                &id,
                "--with-dependents",
                "mods/LibX.jar"
            ])
            .status
            .success()
    );

    let output = sandbox.run(&[
        "mod",
        "enable",
        "--instance",
        &id,
        "--with-dependencies",
        "mods/SkyGUIs.jar",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        jars(&mods),
        [
            "LibX.jar",
            "SkyGUIs.jar",
            "SkyblockBuilder.jar",
            "UtilitiX.jar.disabled",
        ],
        "the addon needs the library and the builder, and UtilitiX is nobody's business"
    );
}

#[test]
fn turning_the_warning_off_applies_the_cascade_without_being_asked() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    // Reading a configuration never writes one, so a setting change is what
    // puts the file on disk for this test to edit.
    assert!(
        sandbox
            .run(&["mod", "catalog", "modrinth"])
            .status
            .success()
    );
    let config = sandbox
        .base()
        .join("config")
        .join("ephervex")
        .join("config.toml");
    let document = std::fs::read_to_string(&config).expect("config");
    assert!(
        document.contains("warn_on_dependency_impact = true"),
        "warning is the default a saved configuration records: {document}"
    );
    std::fs::write(
        &config,
        document.replace(
            "warn_on_dependency_impact = true",
            "warn_on_dependency_impact = false",
        ),
    )
    .expect("write config");

    let output = sandbox.run(&["mod", "disable", "--instance", &id, "mods/LibX.jar"]);
    assert!(
        output.status.success(),
        "suppressing the question accepts it: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        jars(&mods),
        [
            "LibX.jar.disabled",
            "SkyGUIs.jar.disabled",
            "SkyblockBuilder.jar.disabled",
            "UtilitiX.jar.disabled",
        ],
        "leaving the dependents loading would be the broken set the warning exists to prevent"
    );
}
