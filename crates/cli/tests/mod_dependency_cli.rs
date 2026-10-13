//! Turning a mod off, and on, when other mods depend on it.
//!
//! A command has no way to ask, so it refuses instead. That is the whole point
//! of this file: the refusal has to change nothing and has to say so with its
//! exit status, or a script walks straight into a mod set that does not load.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Output;

use tempfile::TempDir;

mod binary_sandbox;
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
fn deleting_a_library_refuses_and_changes_nothing() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    let before = jars(&mods);

    let output = sandbox.run(&["mod", "delete", "--instance", &id, "mods/LibX.jar"]);
    assert!(
        !output.status.success(),
        "a refusal has to fail the command"
    );
    assert_eq!(jars(&mods), before, "nothing on disk may change");
}

#[test]
fn deleting_with_dependents_takes_every_jar_that_needed_it() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();

    let output = sandbox.run(&[
        "mod",
        "delete",
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
    assert!(
        jars(&mods).is_empty(),
        "every jar the cascade named is gone"
    );
}

#[test]
fn a_deleted_jar_leaves_a_record_that_still_wants_it() {
    // Deleting is not removing: the instance still says it should have these
    // mods, which is what makes putting them back possible at all.
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    assert!(
        sandbox
            .run(&[
                "mod",
                "delete",
                "--instance",
                &id,
                "--only-this",
                "mods/UtilitiX.jar"
            ])
            .status
            .success()
    );
    assert!(!jars(&mods).contains(&"UtilitiX.jar".to_owned()));

    let recipe = std::fs::read_to_string(
        mods.parent()
            .expect("game dir")
            .parent()
            .expect("root")
            .join(".launcher")
            .join("source")
            .join("recipe.json"),
    )
    .expect("recipe");
    assert!(
        recipe.contains("mods/UtilitiX.jar"),
        "the record outlives the file"
    );
}

#[test]
fn turning_the_warning_off_applies_the_cascade_without_being_asked() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    set_config(
        &sandbox,
        "warn_on_dependency_impact = true",
        "warn_on_dependency_impact = false",
    );

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

#[test]
fn not_following_relations_turns_off_exactly_the_mod_that_was_named() {
    // Arranging a mod set by hand is a thing this launcher is for, and a
    // launcher that keeps carrying other mods along is in the way of it.
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    set_config(
        &sandbox,
        "follow_mod_dependencies = true",
        "follow_mod_dependencies = false",
    );

    let output = sandbox.run(&["mod", "disable", "--instance", &id, "mods/LibX.jar"]);
    assert!(
        output.status.success(),
        "there is nothing to refuse when nothing else is being changed: {}",
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
        "only the named mod was turned off"
    );
}

#[test]
fn an_explicit_flag_still_carries_the_dependents_when_relations_are_not_followed() {
    // The preference is what happens by default. A flag given on this command
    // is an instruction for this run.
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    set_config(
        &sandbox,
        "follow_mod_dependencies = true",
        "follow_mod_dependencies = false",
    );

    assert!(
        sandbox
            .run(&[
                "mod",
                "disable",
                "--instance",
                &id,
                "mods/LibX.jar",
                "--with-dependents",
            ])
            .status
            .success()
    );
    assert_eq!(
        jars(&mods),
        [
            "LibX.jar.disabled",
            "SkyGUIs.jar.disabled",
            "SkyblockBuilder.jar.disabled",
            "UtilitiX.jar.disabled",
        ],
    );
}

#[test]
fn the_whole_graph_is_listed_in_both_directions() {
    let sandbox = Sandbox::new();
    let (id, _mods) = sandbox.instance_with_a_shared_library();

    let output = sandbox.run(&["mod", "dependencies", "--instance", &id]);
    assert!(output.status.success());
    let printed = String::from_utf8_lossy(&output.stdout);

    assert!(
        printed.contains("mods/LibX.jar\n  needed by  required     mods/SkyblockBuilder.jar"),
        "the library has to name what needs it: {printed}"
    );
    assert!(
        printed.contains("needs      optional     mods/SkyGUIs.jar"),
        "an optional relation is shown as its own kind rather than left out: {printed}"
    );
    assert!(
        printed.contains("mods/UtilitiX.jar\n  needs      required     mods/LibX.jar"),
        "every mod gets its own block: {printed}"
    );
}

#[test]
fn one_mod_can_be_asked_about_on_its_own() {
    let sandbox = Sandbox::new();
    let (id, _mods) = sandbox.instance_with_a_shared_library();

    let output = sandbox.run(&[
        "mod",
        "dependencies",
        "--instance",
        &id,
        "--of",
        "mods/UtilitiX.jar",
    ]);
    assert!(output.status.success());
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(printed.contains("mods/UtilitiX.jar"));
    assert!(
        !printed.contains("mods/SkyGUIs.jar"),
        "asking about one mod answers about that mod: {printed}"
    );
}

#[test]
fn a_mod_nothing_has_asked_about_says_so_rather_than_appearing_independent() {
    // The distinction the whole feature exists for: an empty list because the
    // catalog said "nothing", and an empty list because nobody asked, must not
    // print the same.
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    std::fs::write(mods.join("HandPlaced.jar"), b"jar").expect("write jar");

    let output = sandbox.run(&["mod", "dependencies", "--instance", &id]);
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(
        printed.contains("mods/HandPlaced.jar\n  not asked"),
        "a jar nothing records must not read as one that needs nothing: {printed}"
    );
    assert!(
        printed.contains("mods/LibX.jar\n  needed by"),
        "and the recorded mods still answer: {printed}"
    );
}

#[test]
fn the_json_form_carries_every_relation_with_its_kind_and_state() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    assert!(
        sandbox
            .run(&[
                "mod",
                "disable",
                "--instance",
                &id,
                "mods/UtilitiX.jar",
                "--only-this",
            ])
            .status
            .success()
    );
    assert!(mods.join("UtilitiX.jar.disabled").is_file());

    let output = sandbox.run(&[
        "--json",
        "mod",
        "dependencies",
        "--instance",
        &id,
        "--of",
        "mods/LibX.jar",
    ]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    let entry = &value[0];
    assert_eq!(entry["path"], "mods/LibX.jar");
    assert_eq!(entry["knowledge"], "recorded");
    assert!(
        entry["requires"].as_array().expect("requires").is_empty(),
        "the library needs nothing, and its catalog said so"
    );

    let required_by = entry["required_by"].as_array().expect("required_by");
    assert_eq!(required_by.len(), 3);
    let utility = required_by
        .iter()
        .find(|row| row["path"] == "mods/UtilitiX.jar")
        .expect("the utility");
    assert_eq!(utility["kind"], "required");
    assert_eq!(
        utility["presence"], "disabled",
        "a dependent that is turned off is still a dependent, and says which it is"
    );
}

/// Adds a recorded mod to the instance, leaving its jar turned on.
fn add_recorded_mod(mods: &Path, name: &str, project: &str, relations: &[(&str, &str)]) {
    let recipe_path = mods
        .parent()
        .and_then(Path::parent)
        .expect("instance root")
        .join(".launcher")
        .join("source")
        .join("recipe.json");
    let mut recipe: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&recipe_path).expect("recipe")).expect("json");
    recipe["files"]
        .as_array_mut()
        .expect("files")
        .push(entry(name, project, relations));
    std::fs::write(
        &recipe_path,
        serde_json::to_vec(&recipe).expect("serialize"),
    )
    .expect("write recipe");
    std::fs::write(mods.join(name), b"jar").expect("write jar");
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn disabling_a_mod_others_only_use_optionally_refuses_and_changes_nothing() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    let before = jars(&mods);

    let output = sandbox.run(&["mod", "disable", "--instance", &id, "mods/SkyGUIs.jar"]);
    assert!(!output.status.success(), "an optional user is still a user");
    let stderr = stderr_of(&output);
    assert!(stderr.contains("SkyblockBuilder.jar"), "{stderr}");
    assert!(
        !stderr.contains("need this one"),
        "nothing was proven to need it: {stderr}"
    );
    assert_eq!(jars(&mods), before, "nothing on disk may change");

    for flag in ["--only-this", "--with-dependents"] {
        let allowed = sandbox.run(&[
            "mod",
            "disable",
            "--instance",
            &id,
            flag,
            "mods/SkyGUIs.jar",
        ]);
        assert!(allowed.status.success(), "{flag}: {}", stderr_of(&allowed));
        assert!(
            jars(&mods).contains(&"SkyGUIs.jar.disabled".to_owned()),
            "{flag} is the instruction to go ahead"
        );
        assert!(
            jars(&mods).contains(&"SkyblockBuilder.jar".to_owned()),
            "an optional user is never carried along"
        );
        std::fs::rename(mods.join("SkyGUIs.jar.disabled"), mods.join("SkyGUIs.jar"))
            .expect("restore");
    }
}

#[test]
fn disabling_a_mod_while_a_jar_nothing_records_is_installed_refuses() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    std::fs::write(mods.join("HandPlaced.jar"), b"jar").expect("write jar");
    let before = jars(&mods);

    let output = sandbox.run(&[
        "--json",
        "mod",
        "disable",
        "--instance",
        &id,
        "mods/UtilitiX.jar",
    ]);
    assert!(
        !output.status.success(),
        "whether the unrecorded jar needs it is not known"
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("impact json");
    assert!(value["breaks"].as_array().expect("breaks").is_empty());
    let unanswered: Vec<&str> = ["unknown", "unidentifiable"]
        .iter()
        .flat_map(|key| value[key].as_array().expect("a list"))
        .map(|entry| entry["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(unanswered, ["HandPlaced.jar"]);
    assert!(!value["complete"].as_bool().expect("complete"));
    assert_eq!(jars(&mods), before, "nothing on disk may change");

    let allowed = sandbox.run(&[
        "mod",
        "disable",
        "--instance",
        &id,
        "--only-this",
        "mods/UtilitiX.jar",
    ]);
    assert!(allowed.status.success(), "{}", stderr_of(&allowed));
}

#[test]
fn the_refusal_advice_carries_only_the_mods_that_need_the_target() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    std::fs::write(mods.join("HandPlaced.jar"), b"jar").expect("write jar");

    for verb in ["disable", "delete"] {
        let output = sandbox.run(&["mod", verb, "--instance", &id, "mods/LibX.jar"]);
        assert!(!output.status.success());
        let stderr = stderr_of(&output);
        assert!(stderr.contains("Mods that need this one: 3."), "{stderr}");
        assert!(stderr.contains("no recorded requirements"), "{stderr}");
        assert!(
            stderr.contains("the mods that need it too"),
            "the flag is described by what it takes along: {stderr}"
        );
    }
}

#[test]
fn deleting_a_mod_while_a_jar_nothing_records_is_installed_refuses() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    std::fs::write(mods.join("HandPlaced.jar"), b"jar").expect("write jar");
    let before = jars(&mods);

    let output = sandbox.run(&["mod", "delete", "--instance", &id, "mods/UtilitiX.jar"]);
    assert!(!output.status.success());
    let stderr = stderr_of(&output);
    assert!(stderr.contains("HandPlaced.jar"), "{stderr}");
    assert_eq!(jars(&mods), before, "nothing on disk may change");

    let allowed = sandbox.run(&[
        "mod",
        "delete",
        "--instance",
        &id,
        "--only-this",
        "mods/UtilitiX.jar",
    ]);
    assert!(allowed.status.success(), "{}", stderr_of(&allowed));
    assert!(!jars(&mods).contains(&"UtilitiX.jar".to_owned()));
}

#[test]
fn enabling_a_mod_whose_requirement_is_not_installed_refuses() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    add_recorded_mod(
        &mods,
        "NeedsMissing.jar",
        "zzMissing1",
        &[("zzAbsent01", "required")],
    );
    let disabled = sandbox.run(&[
        "mod",
        "disable",
        "--instance",
        &id,
        "--only-this",
        "mods/NeedsMissing.jar",
    ]);
    assert!(disabled.status.success(), "{}", stderr_of(&disabled));
    let before = jars(&mods);

    let output = sandbox.run(&["mod", "enable", "--instance", &id, "mods/NeedsMissing.jar"]);
    assert!(
        !output.status.success(),
        "a mod enabled without what it needs does not load"
    );
    assert!(
        stderr_of(&output).contains("zzAbsent01"),
        "{}",
        stderr_of(&output)
    );
    assert_eq!(jars(&mods), before, "nothing on disk may change");

    let allowed = sandbox.run(&[
        "mod",
        "enable",
        "--instance",
        &id,
        "--only-this",
        "mods/NeedsMissing.jar",
    ]);
    assert!(allowed.status.success(), "{}", stderr_of(&allowed));
    assert!(
        stderr_of(&allowed).contains("zzAbsent01"),
        "the missing mod is still reported: {}",
        stderr_of(&allowed)
    );
    assert!(jars(&mods).contains(&"NeedsMissing.jar".to_owned()));
}

#[test]
fn enabling_a_mod_declared_incompatible_with_a_loaded_one_refuses() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    add_recorded_mod(
        &mods,
        "Clashing.jar",
        "zzClash001",
        &[(LIBRARY, "incompatible")],
    );
    let disabled = sandbox.run(&[
        "mod",
        "disable",
        "--instance",
        &id,
        "--only-this",
        "mods/Clashing.jar",
    ]);
    assert!(disabled.status.success(), "{}", stderr_of(&disabled));
    let before = jars(&mods);

    let output = sandbox.run(&[
        "--json",
        "mod",
        "enable",
        "--instance",
        &id,
        "mods/Clashing.jar",
    ]);
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("needs json");
    let incompatible: Vec<&str> = value["incompatible"]
        .as_array()
        .expect("incompatible")
        .iter()
        .map(|entry| entry["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(incompatible, ["LibX.jar"]);
    assert_eq!(jars(&mods), before, "nothing on disk may change");

    let allowed = sandbox.run(&[
        "mod",
        "enable",
        "--instance",
        &id,
        "--only-this",
        "mods/Clashing.jar",
    ]);
    assert!(allowed.status.success(), "{}", stderr_of(&allowed));
    assert!(
        stderr_of(&allowed).contains("LibX.jar"),
        "the conflict is still reported: {}",
        stderr_of(&allowed)
    );
    assert!(jars(&mods).contains(&"Clashing.jar".to_owned()));
}

#[test]
fn an_explicit_flag_reports_missing_requirements_when_relations_are_not_followed() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    add_recorded_mod(
        &mods,
        "NeedsMissing.jar",
        "zzMissing1",
        &[("zzAbsent01", "required")],
    );
    let disabled = sandbox.run(&[
        "mod",
        "disable",
        "--instance",
        &id,
        "--only-this",
        "mods/NeedsMissing.jar",
    ]);
    assert!(disabled.status.success(), "{}", stderr_of(&disabled));
    set_config(
        &sandbox,
        "follow_mod_dependencies = true",
        "follow_mod_dependencies = false",
    );

    let output = sandbox.run(&[
        "mod",
        "enable",
        "--instance",
        &id,
        "--with-dependencies",
        "mods/NeedsMissing.jar",
    ]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(jars(&mods).contains(&"NeedsMissing.jar".to_owned()));
    assert!(
        stderr_of(&output).contains("zzAbsent01"),
        "the requirement that is not installed is reported: {}",
        stderr_of(&output)
    );
}

#[test]
fn an_explicit_flag_reports_incompatible_mods_when_relations_are_not_followed() {
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    add_recorded_mod(
        &mods,
        "Clashing.jar",
        "zzClash001",
        &[(LIBRARY, "incompatible")],
    );
    let disabled = sandbox.run(&[
        "mod",
        "disable",
        "--instance",
        &id,
        "--only-this",
        "mods/Clashing.jar",
    ]);
    assert!(disabled.status.success(), "{}", stderr_of(&disabled));
    set_config(
        &sandbox,
        "follow_mod_dependencies = true",
        "follow_mod_dependencies = false",
    );

    let output = sandbox.run(&[
        "mod",
        "enable",
        "--instance",
        &id,
        "--with-dependencies",
        "mods/Clashing.jar",
    ]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(jars(&mods).contains(&"Clashing.jar".to_owned()));
    assert!(
        stderr_of(&output).contains("LibX.jar"),
        "the conflicting mod is reported: {}",
        stderr_of(&output)
    );
}

#[test]
fn not_following_relations_does_not_report_requirements_for_an_unflagged_enable() {
    // The report accompanies carrying dependencies along; with the preference
    // off and no flag, the command touches only the named mod.
    let sandbox = Sandbox::new();
    let (id, mods) = sandbox.instance_with_a_shared_library();
    add_recorded_mod(
        &mods,
        "NeedsMissing.jar",
        "zzMissing1",
        &[("zzAbsent01", "required")],
    );
    let disabled = sandbox.run(&[
        "mod",
        "disable",
        "--instance",
        &id,
        "--only-this",
        "mods/NeedsMissing.jar",
    ]);
    assert!(disabled.status.success(), "{}", stderr_of(&disabled));
    set_config(
        &sandbox,
        "follow_mod_dependencies = true",
        "follow_mod_dependencies = false",
    );

    let output = sandbox.run(&["mod", "enable", "--instance", &id, "mods/NeedsMissing.jar"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(
        !stderr_of(&output).contains("zzAbsent01"),
        "{}",
        stderr_of(&output)
    );
}

/// Rewrites one recorded setting, through a save the launcher itself made.
///
/// Reading a configuration never writes one, so a setting change is what puts
/// the file on disk to edit.
fn set_config(sandbox: &Sandbox, from: &str, to: &str) {
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
        document.contains(from),
        "expected {from} in the saved configuration: {document}"
    );
    std::fs::write(&config, document.replace(from, to)).expect("write config");
}
