//! Command-line coverage for installed managed mods, offline.
//!
//! Searching and installing need a provider, so those are covered where a
//! loopback server can stand in for one (`crates/mod-platform/tests/` and
//! `crates/pack/tests/`). What only a separate `ephervex` process can prove is
//! what these cover: that a recipe written by one run is read correctly by the
//! next, and that a jar nobody recorded stays invisible to the mod list.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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

    /// Creates a Forge instance and returns its id and root.
    fn instance(&self) -> (String, PathBuf) {
        let created = self.json(&[
            "--json",
            "instance",
            "create",
            "Mod browser check",
            "--minecraft",
            "1.20.1",
            "--loader",
            "forge",
            "--loader-version",
            "47.4.10",
        ]);
        let id = created["id"].as_str().expect("an id").to_owned();
        let root = PathBuf::from(created["root"].as_str().expect("a root"));
        (id, root)
    }
}

/// A recipe as an install writes one, seeded directly so this test needs no
/// provider.
fn seed_recipe(root: &Path) {
    let source = root.join(".launcher/source");
    std::fs::create_dir_all(&source).expect("source dir");
    std::fs::write(
        source.join("recipe.json"),
        r#"{
          "recipe_version": 1,
          "source_kind": "manual",
          "original_name": "Mod browser check",
          "minecraft_version": "1.20.1",
          "loader": {"kind": "forge", "version": "47.4.10"},
          "files": [
            {
              "logical_path": "mods/library-1.0.0.jar",
              "hashes": [{"algorithm": "sha1", "digest": "da39a3ee5e6b4b0d3255bfef95601890afd80709"}],
              "downloads": ["https://cdn.modrinth.com/data/LIBPROJ/library-1.0.0.jar"],
              "client": "required",
              "server": "unknown",
              "origin": "required_dependency",
              "identity": {
                "provider": "modrinth",
                "project_id": "LIBPROJ",
                "version_id": "LIBV1"
              }
            },
            {
              "logical_path": "mods/chosen-2.0.0.jar",
              "hashes": [{"algorithm": "sha1", "digest": "da39a3ee5e6b4b0d3255bfef95601890afd80709"}],
              "downloads": ["https://cdn.modrinth.com/data/ROOTPROJ/chosen-2.0.0.jar"],
              "client": "required",
              "server": "required",
              "origin": "user_selected",
              "identity": {
                "provider": "modrinth",
                "project_id": "ROOTPROJ",
                "version_id": "ROOTV2"
              }
            }
          ],
          "provider_metadata": {"kind": "manual"}
        }"#,
    )
    .expect("write recipe");
}

#[test]
fn installed_mods_are_reported_by_a_process_that_did_not_install_them() {
    // The whole point of recording provider metadata: reopening Ephervex still
    // knows what each jar is, without reading the jar.
    let sandbox = Sandbox::new();
    let (id, root) = sandbox.instance();
    seed_recipe(&root);

    let listed = sandbox.json(&["--json", "mod", "list", &id]);
    let mods = listed["mods"].as_array().expect("an array");
    assert_eq!(mods.len(), 2);

    assert_eq!(mods[0]["provider"], "modrinth");
    assert_eq!(mods[0]["project_id"], "LIBPROJ");
    assert_eq!(mods[0]["version_id"], "LIBV1");
    assert_eq!(mods[0]["file_name"], "library-1.0.0.jar");
    assert_eq!(mods[0]["origin"], "required_dependency");
    assert!(
        !mods[0]["hashes"].as_array().expect("hashes").is_empty(),
        "the verified hashes survive a reopen"
    );

    assert_eq!(mods[1]["project_id"], "ROOTPROJ");
    assert_eq!(
        mods[1]["origin"], "user_selected",
        "the mod the user chose is distinguishable from the one pulled in for it"
    );
}

#[test]
fn a_jar_dropped_into_the_mods_folder_is_not_reported_as_an_installed_mod() {
    // Nothing scans `mods/`. A jar with no provenance stays opaque rather than
    // being given a record Ephervex cannot resolve again.
    let sandbox = Sandbox::new();
    let (id, root) = sandbox.instance();
    seed_recipe(&root);

    let mods_dir = root.join("minecraft/mods");
    std::fs::create_dir_all(&mods_dir).expect("mods dir");
    std::fs::write(mods_dir.join("my-local-build.jar"), b"not ours").expect("write");

    let listed = sandbox.json(&["--json", "mod", "list", &id]);
    let names: Vec<&str> = listed["mods"]
        .as_array()
        .expect("an array")
        .iter()
        .map(|entry| entry["file_name"].as_str().expect("a name"))
        .collect();
    assert_eq!(names, vec!["library-1.0.0.jar", "chosen-2.0.0.jar"]);
    assert!(!names.contains(&"my-local-build.jar"));
}

#[test]
fn an_instance_with_no_installed_mods_says_so_rather_than_failing() {
    let sandbox = Sandbox::new();
    let (id, _root) = sandbox.instance();

    let listed = sandbox.json(&["--json", "mod", "list", &id]);
    assert!(listed["mods"].as_array().expect("an array").is_empty());

    let output = sandbox.run(&["mod", "list", &id]);
    assert!(output.status.success());
    assert!(
        !String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "an empty list is still an answer"
    );
}

#[test]
fn a_pack_declared_file_is_not_listed_as_a_provider_installed_mod() {
    // An imported pack's files are in the recipe and are installed, but the mod
    // browser does not own them: there is no project or version to re-resolve.
    let sandbox = Sandbox::new();
    let (id, root) = sandbox.instance();
    let source = root.join(".launcher/source");
    std::fs::create_dir_all(&source).expect("source dir");
    std::fs::write(
        source.join("recipe.json"),
        r#"{
          "recipe_version": 1,
          "source_kind": "modrinth_pack",
          "original_name": "Imported Pack",
          "minecraft_version": "1.20.1",
          "loader": {"kind": "forge", "version": "47.4.10"},
          "files": [
            {
              "logical_path": "mods/from-the-pack.jar",
              "hashes": [{"algorithm": "sha1", "digest": "da39a3ee5e6b4b0d3255bfef95601890afd80709"}],
              "downloads": ["https://cdn.modrinth.com/data/x/from-the-pack.jar"],
              "client": "required",
              "server": "required"
            }
          ],
          "provider_metadata": {"kind": "modrinth_pack"}
        }"#,
    )
    .expect("write recipe");

    let listed = sandbox.json(&["--json", "mod", "list", &id]);
    assert!(
        listed["mods"].as_array().expect("an array").is_empty(),
        "a pack file has no project or version, so it is not a managed mod"
    );

    // ...and it is still in the recipe, and still part of the server plan.
    let recipe = sandbox.json(&["--json", "instance", "recipe", &id, "--side", "server"]);
    assert_eq!(recipe["files"].as_array().expect("an array").len(), 1);
}

#[test]
fn a_recipe_from_a_newer_build_is_refused_rather_than_partly_read() {
    let sandbox = Sandbox::new();
    let (id, root) = sandbox.instance();
    seed_recipe(&root);

    let path = root.join(".launcher/source/recipe.json");
    let text = std::fs::read_to_string(&path)
        .expect("read")
        .replace("\"recipe_version\": 1", "\"recipe_version\": 99");
    std::fs::write(&path, text).expect("write");

    let output = sandbox.run(&["--json", "mod", "list", &id]);
    assert!(!output.status.success());
    // Failures go to stderr so that `--json` output on stdout stays valid.
    let failure: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("a machine-readable failure");
    assert_eq!(
        failure["message_id"], "error-pack-recipe-unsupported-version",
        "a recipe this build cannot fully read is refused rather than partly listed"
    );
}

#[test]
fn a_catalog_can_be_chosen_and_an_unknown_one_is_refused() {
    // The catalog is part of the request rather than an application-wide
    // setting, and only the ones this build can browse are accepted.
    let sandbox = Sandbox::new();
    let help = sandbox.run(&["mod", "search", "--help"]);
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("--provider"), "{text}");
    assert!(text.contains("modrinth"), "{text}");
    assert!(
        text.contains("curse-forge") || text.contains("curseforge"),
        "{text}"
    );

    let refused = sandbox.run(&[
        "mod",
        "search",
        "00000000-0000-4000-8000-000000000000",
        "jei",
        "--provider",
        "nowhere",
    ]);
    assert!(!refused.status.success());
}
