//! Durable mod isolation exercised through fresh CLI processes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Output;

use ephervex_model::{APP_ID, InstanceId, ServerInstallation, ServerLaunch, ServerRelativePath};
use ephervex_storage::{NewServer, Paths, Registry};
use serde_json::json;
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
            .expect("run ephervex")
    }

    /// The roots the child process resolves from the environment above.
    fn paths(&self) -> Paths {
        use std::os::unix::fs::MetadataExt;
        let base = self.directory.path();
        let uid = std::fs::metadata(base).expect("sandbox metadata").uid();
        Paths::from_roots(
            base.join("config").join(APP_ID),
            base.join("data").join(APP_ID),
            base.join("cache").join(APP_ID),
            base.join("state").join(APP_ID),
            base.join("tmp").join(format!("{APP_ID}-{uid}")),
        )
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
    seed_mods(&root, "minecraft");
    (
        created["id"].as_str().expect("instance id").to_owned(),
        root,
    )
}

/// Declares and writes the test mods into `<root>/<game_dir>/mods`.
fn seed_mods(root: &Path, game_dir: &str) {
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
            &root.join(format!("{game_dir}/mods/{name}.jar")),
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
    enabled_in(root, "minecraft")
}

fn enabled_in(root: &Path, game_dir: &str) -> Vec<String> {
    let mut names = std::fs::read_dir(root.join(game_dir).join("mods"))
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

    let mut step = undone;
    while step["state"] == "awaiting_answer" {
        let reproduces = step["value"]["enabled"]
            .as_array()
            .unwrap()
            .contains(&json!("mods/e.jar"));
        let answer = if reproduces {
            "issue-still-present"
        } else {
            "issue-gone"
        };
        step = sandbox.json(&[
            "--json", "mod", "isolate", "answer", host[0], host[1], answer,
        ]);
    }
    assert_eq!(step["state"], "complete");
    let finished = sandbox.json(&["--json", "mod", "isolate", "finish", host[0], host[1]]);
    assert_eq!(finished["remaining_candidates"][0]["root"], "mods/e.jar");
    assert_eq!(finished["pinned"], json!(["mods/pin.jar"]));
    assert_eq!(finished["pinned_dependencies"], json!(["mods/base.jar"]));
    assert_eq!(finished["required_dependencies"], json!([]));
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
    assert_eq!(canceled["warnings"], json!([]));
    assert_eq!(enabled(&root), before);
    assert!(!root.join(".launcher/isolation.json").exists());
}

#[test]
fn the_cli_refuses_an_answer_for_mods_the_host_no_longer_has_until_resumed() {
    let sandbox = Sandbox::new();
    let (instance, root) = create_instance(&sandbox, "Isolation CLI drift");
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
    let disabled = started["value"]["disabled"][0].as_str().unwrap().to_owned();
    let jar = root.join("minecraft").join(&disabled);
    std::fs::rename(jar.with_extension("jar.disabled"), &jar).unwrap();

    let refused = sandbox.run(&["mod", "isolate", "answer", host[0], host[1], "issue-gone"]);
    assert!(!refused.status.success());
    let status = sandbox.json(&["--json", "mod", "isolate", "status", host[0], host[1]]);
    assert_eq!(status["history"], json!([]));

    let resumed = sandbox.json(&["--json", "mod", "isolate", "resume", host[0], host[1]]);
    assert_eq!(resumed, started);
    assert!(!jar.exists());
    let answered = sandbox.json(&[
        "--json",
        "mod",
        "isolate",
        "answer",
        host[0],
        host[1],
        "issue-gone",
    ]);
    assert_eq!(answered["value"]["index"], 2);
}

/// An installed server linked to `instance`, declaring the same mods.
fn linked_server(sandbox: &Sandbox, instance: &str) -> (String, PathBuf) {
    let registry = Registry::open(sandbox.paths()).expect("open the registry");
    let client: InstanceId = instance.parse().expect("instance id");
    let mut server = registry
        .create_server(NewServer::linked("Linked Example", client))
        .expect("create the server");
    server.installation = ServerInstallation::Installed {
        launch: ServerLaunch::Jar {
            path: ServerRelativePath::new("server.jar").expect("relative"),
        },
    };
    registry.update_server(&mut server).expect("install");
    seed_mods(&server.root, "server");
    (server.id.to_string(), server.root)
}

#[test]
fn the_cli_keeps_a_named_linked_server_in_step_and_cancel_restores_it() {
    let sandbox = Sandbox::new();
    let (instance, root) = create_instance(&sandbox, "Isolation Linked");
    let (server, server_root) = linked_server(&sandbox, &instance);
    let before = enabled_in(&server_root, "server");

    let started = sandbox.run(&[
        "mod",
        "isolate",
        "start",
        "--instance",
        &instance,
        "--linked-server",
        &server,
    ]);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let printed = String::from_utf8_lossy(&started.stdout);
    assert!(
        printed.contains("Linked server Linked Example"),
        "{printed}"
    );
    assert_ne!(enabled(&root), before);
    assert_eq!(enabled_in(&server_root, "server"), enabled(&root));

    let status = sandbox.json(&[
        "--json",
        "mod",
        "isolate",
        "status",
        "--instance",
        &instance,
    ]);
    assert_eq!(status["followed_servers"].as_array().unwrap().len(), 1);
    assert!(
        server_root
            .join(".launcher/isolation-leader.json")
            .is_file()
    );

    let refused = sandbox.run(&[
        "mod",
        "disable",
        "--server",
        &server,
        "mods/a.jar",
        "--only-this",
    ]);
    assert!(!refused.status.success());

    sandbox.json(&[
        "--json",
        "mod",
        "isolate",
        "cancel",
        "--instance",
        &instance,
    ]);
    assert_eq!(enabled_in(&server_root, "server"), before);
    assert!(!server_root.join(".launcher/isolation-leader.json").exists());
}

#[test]
fn the_cli_refuses_a_linked_server_that_is_not_linked_to_the_client() {
    let sandbox = Sandbox::new();
    let (instance, root) = create_instance(&sandbox, "Isolation Unlinked");
    let before = enabled(&root);

    let refused = sandbox.run(&[
        "mod",
        "isolate",
        "start",
        "--instance",
        &instance,
        "--linked-server",
        "00000000-0000-4000-8000-000000000002",
    ]);

    assert!(!refused.status.success());
    assert_eq!(enabled(&root), before);
    assert!(!root.join(".launcher/isolation.json").exists());
}
