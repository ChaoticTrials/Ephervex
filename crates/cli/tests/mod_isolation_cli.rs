//! Durable mod isolation exercised through fresh CLI processes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::json;
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

    fn run(&self, args: &[&str]) -> Output {
        let base = self.directory.path();
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
            .expect("run ephervex")
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

fn create_instance(sandbox: &Sandbox, name: &str) -> (String, PathBuf) {
    let created = sandbox.json(&[
        "--json",
        "instance",
        "create",
        name,
        "--minecraft",
        "1.20.1",
    ]);
    let root = PathBuf::from(created["root"].as_str().expect("instance root"));
    seed_mods(&root);
    (
        created["id"].as_str().expect("instance id").to_owned(),
        root,
    )
}

fn seed_mods(root: &Path) {
    let files = [
        ("a", vec!["shared"]),
        ("b", vec!["shared"]),
        ("c", vec![]),
        ("d", vec![]),
        ("e", vec![]),
        ("pin", vec!["base"]),
        ("shared", vec!["base"]),
        ("base", vec![]),
    ]
    .into_iter()
    .map(|(name, requires)| {
        write(
            &root.join(format!("minecraft/mods/{name}.jar")),
            name.as_bytes(),
        );
        json!({
            "logical_path": format!("mods/{name}.jar"), "hashes": [],
            "downloads": [], "size": name.len(), "client": "required", "server": "required",
            "origin": "user_selected",
            "identity": {"provider": "modrinth", "project_id": name, "version_id": format!("{name}1")},
            "relations": requires.into_iter().map(|dependency| json!({
                "kind": "required", "target": {"project": {"project_id": dependency}}
            })).collect::<Vec<_>>(),
        })
    })
    .collect::<Vec<_>>();
    write(
        &root.join(".launcher/source/recipe.json"),
        &serde_json::to_vec(&json!({
            "recipe_version": 1, "source_kind": "manual", "original_name": "Isolation CLI",
            "minecraft_version": "1.20.1", "loader": {"kind": "vanilla", "version": null},
            "files": files, "override_layers": [], "provider_metadata": {"kind": "manual"}
        }))
        .unwrap(),
    );
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    std::fs::write(path, bytes).expect("write file");
}

fn enabled(root: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(root.join("minecraft/mods"))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn the_cli_resumes_undoes_and_finishes_a_complete_isolation_session() {
    let sandbox = Sandbox::new();
    let (instance, root) = create_instance(&sandbox, "Isolation CLI");
    let host = ["--instance", instance.as_str()];

    let started = sandbox.json(&[
        "--json",
        "mod",
        "isolate",
        "start",
        host[0],
        host[1],
        "--keep",
        "mods/pin.jar",
    ]);
    assert_eq!(started["state"], "awaiting_answer");
    assert_eq!(started["value"]["index"], 1);

    let status = sandbox.json(&["--json", "mod", "isolate", "status", host[0], host[1]]);
    assert_eq!(status["current_round"]["index"], 1);
    assert!(status["safety_snapshot"].as_str().is_some());

    let resumed = sandbox.json(&["--json", "mod", "isolate", "resume", host[0], host[1]]);
    assert_eq!(resumed, started);

    let second = sandbox.json(&[
        "--json",
        "mod",
        "isolate",
        "answer",
        host[0],
        host[1],
        "issue-gone",
    ]);
    assert_eq!(second["value"]["index"], 2);
    let undone = sandbox.json(&["--json", "mod", "isolate", "undo", host[0], host[1]]);
    assert_eq!(undone, started);

    for answer in [
        "issue-gone",
        "issue-still-present",
        "issue-gone",
        "issue-still-present",
    ] {
        sandbox.json(&[
            "--json", "mod", "isolate", "answer", host[0], host[1], answer,
        ]);
    }
    let finished = sandbox.json(&["--json", "mod", "isolate", "finish", host[0], host[1]]);
    assert_eq!(finished["reproducing"][0]["root"], "mods/e.jar");
    assert_eq!(finished["confidence"], "smallest_reached");
    assert_eq!(enabled(&root), ["base.jar", "e.jar", "pin.jar"]);
    assert!(!root.join(".launcher/isolation.json").exists());
    assert_eq!(
        sandbox
            .json(&["--json", "snapshot", "list", host[0], host[1]])
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn the_cli_cancel_restores_the_pre_isolation_configuration() {
    let sandbox = Sandbox::new();
    let (instance, root) = create_instance(&sandbox, "Isolation Cancel");
    let before = enabled(&root);
    sandbox.json(&["--json", "mod", "isolate", "start", "--instance", &instance]);
    assert_ne!(enabled(&root), before);
    let canceled = sandbox.json(&[
        "--json",
        "mod",
        "isolate",
        "cancel",
        "--instance",
        &instance,
    ]);
    assert_eq!(canceled["canceled"], true);
    assert_eq!(enabled(&root), before);
    assert!(!root.join(".launcher/isolation.json").exists());
}
