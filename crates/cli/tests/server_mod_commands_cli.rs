//! What the mod commands say when the host they act on is a server.
//!
//! The server record is written through the registry the launcher reads, into
//! the same sandboxed roots the child process resolves, so nothing here needs a
//! network or an installed server.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Output;

use ephervex_model::{APP_ID, LoaderSelection, StorageClass};
use ephervex_storage::{NewServer, Paths, Registry};
use tempfile::TempDir;

mod binary_sandbox;
mod support;

/// Ids that parse but name nothing, for checks that stop at the arguments.
const SOME_INSTANCE: &str = "00000000-0000-4000-8000-000000000001";
const SOME_SERVER: &str = "00000000-0000-4000-8000-000000000002";

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

    /// The roots the child process resolves from the environment above.
    fn paths(&self) -> Paths {
        use std::os::unix::fs::MetadataExt;
        let base = self.base();
        let uid = std::fs::metadata(base).expect("sandbox metadata").uid();
        Paths::from_roots(
            base.join("config").join(APP_ID),
            base.join("data").join(APP_ID),
            base.join("cache").join(APP_ID),
            base.join("state").join(APP_ID),
            base.join("tmp").join(format!("{APP_ID}-{uid}")),
        )
    }

    /// Records a persistent vanilla server and returns its id and root.
    fn server(&self) -> (String, PathBuf) {
        let server = Registry::open(self.paths())
            .expect("open the registry")
            .create_server(
                NewServer::new("Server Example", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create the server");
        (server.id.to_string(), server.root)
    }
}

/// Writes a recipe recording `files` into a server's source store.
fn seed_recipe(root: &Path, files: &[serde_json::Value]) {
    let recipe = serde_json::json!({
        "recipe_version": 1,
        "source_kind": "manual",
        "original_name": "Server Example",
        "minecraft_version": "1.20.1",
        "loader": {"kind": "vanilla"},
        "files": files,
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
}

/// A recorded server mod, fetched from `downloads`.
fn entry(name: &str, downloads: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "logical_path": format!("mods/{name}"),
        "hashes": [{"algorithm": "sha1", "digest": "da39a3ee5e6b4b0d3255bfef95601890afd80709"}],
        "downloads": downloads,
        "size": 0,
        "client": "required",
        "server": "required",
        "origin": "user_selected",
    })
}

fn mods_dir(root: &Path) -> PathBuf {
    let mods = root.join("server").join("mods");
    std::fs::create_dir_all(&mods).expect("mods dir");
    mods
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_server_with_no_mods_says_so_in_server_terms() {
    let sandbox = Sandbox::new();
    let (id, _) = sandbox.server();

    let output = sandbox.run(&["server", "mods", &id]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    let stdout = stdout_of(&output);
    assert!(stdout.contains("This server has no mods."), "{stdout}");
    assert!(!stdout.contains("instance"), "{stdout}");
}

#[test]
fn the_server_mod_count_separates_loaded_disabled_and_missing_mods() {
    let sandbox = Sandbox::new();
    let (id, root) = sandbox.server();
    seed_recipe(&root, &[entry("gone.jar", &[])]);
    let mods = mods_dir(&root);
    std::fs::write(mods.join("loads.jar"), b"jar").expect("write jar");
    std::fs::write(mods.join("resting.jar.disabled"), b"jar").expect("write jar");

    let output = sandbox.run(&["server", "mods", &id]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("1 installed, 1 disabled, 1 missing"),
        "{stdout}"
    );
}

#[test]
fn server_mods_json_gives_no_name_rather_than_the_file_name_when_no_catalog_described_the_mod() {
    let sandbox = Sandbox::new();
    let (id, root) = sandbox.server();
    let mut recorded = entry("library-1.0.0.jar", &[]);
    recorded["identity"] = serde_json::json!({
        "provider": "modrinth",
        "project_id": "LIBPROJ",
        "version_id": "LIBV1",
    });
    seed_recipe(&root, &[recorded]);
    std::fs::write(mods_dir(&root).join("library-1.0.0.jar"), b"").expect("write jar");

    let output = sandbox.run(&["--json", "server", "mods", &id]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    let row = &value["mods"][0];
    assert_eq!(row["file_name"], "library-1.0.0.jar");
    assert!(
        row["name"].is_null(),
        "no provider has named this mod, so the file name is not its name: {row}"
    );
    assert!(row["version"].is_null(), "{row}");
    assert_eq!(row["provider"], "modrinth");
    assert_eq!(row["project_id"], "LIBPROJ");
    assert_eq!(row["version_id"], "LIBV1");
}

#[test]
fn verifying_a_server_reports_the_fault_as_the_servers() {
    let sandbox = Sandbox::new();
    let (id, root) = sandbox.server();
    seed_recipe(
        &root,
        &[entry(
            "fetched.jar",
            &["https://cdn.modrinth.com/data/FETCHED/fetched.jar"],
        )],
    );
    mods_dir(&root);

    let output = sandbox.run(&["mod", "verify", "--server", &id]);
    assert!(!output.status.success(), "a missing file is a fault");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("this server does not hold what it recorded"),
        "{stderr}"
    );
    assert!(!stderr.contains("instance"), "{stderr}");
}

#[test]
fn repairing_a_server_points_at_the_server_folder_for_a_file_with_no_address() {
    let sandbox = Sandbox::new();
    let (id, root) = sandbox.server();
    seed_recipe(&root, &[entry("shipped.jar", &[])]);
    mods_dir(&root);

    let output = sandbox.run(&["mod", "repair", "--server", &id]);
    assert!(!output.status.success(), "the file is still missing");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("put the file back in the server folder by hand"),
        "{stderr}"
    );
    assert!(!stderr.contains("re-import the pack"), "{stderr}");
    assert!(
        stderr.contains("this server still does not hold what it recorded"),
        "{stderr}"
    );
}

#[test]
fn server_commands_describe_the_server_argument_in_their_help() {
    let sandbox = Sandbox::new();
    for command in ["mods", "delete"] {
        let output = sandbox.run(&["server", command, "-h"]);
        assert!(output.status.success(), "{}", stderr_of(&output));
        let stdout = stdout_of(&output);
        let argument = stdout
            .lines()
            .find(|line| line.trim_start().starts_with("<ID>"))
            .unwrap_or_else(|| panic!("no <ID> argument in:\n{stdout}"));
        assert!(
            argument.trim_start().len() > "<ID>".len(),
            "`server {command}` leaves <ID> without help:\n{stdout}"
        );
    }
}

#[test]
fn mod_install_usage_names_both_kinds_of_host() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["mod", "install", "--help"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    let usage = stdout_of(&output)
        .lines()
        .find(|line| line.starts_with("Usage:"))
        .expect("a usage line")
        .to_owned();
    assert!(
        usage.contains("--instance") && usage.contains("--server"),
        "{usage}"
    );

    let neither = sandbox.run(&["mod", "install", "sodium"]);
    assert!(!neither.status.success());
    let both = sandbox.run(&[
        "mod",
        "install",
        "--instance",
        SOME_INSTANCE,
        "--server",
        SOME_SERVER,
        "sodium",
    ]);
    assert!(!both.status.success());
    assert!(
        stderr_of(&both).contains("cannot be used with"),
        "{}",
        stderr_of(&both)
    );
}

#[test]
fn a_search_limit_of_zero_is_refused() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&[
        "mod",
        "search",
        "--instance",
        SOME_INSTANCE,
        "--limit",
        "0",
        "sodium",
    ]);
    assert!(!output.status.success());
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("--limit") && stderr.contains("expected at least 1"),
        "{stderr}"
    );
}
