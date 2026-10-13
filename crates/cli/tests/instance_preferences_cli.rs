//! The instance and preference management regression coverage, driven through the `ephervex` binary.
//!
//! > Creating two instances with different Minecraft versions applies the same
//! > normalized GUI preference correctly without copying a foreign
//! > `options.txt`.
//! >
//! > Editing one instance afterward does not mutate the global settings or
//! > another instance.
//!
//! Same sandbox arrangement as the storage and registry support suite: every run is pointed at a
//! throwaway directory through the XDG variables and `TMPDIR`, set on a *child*
//! process, so nothing here touches the developer's real instances and nothing
//! here needs the network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Output;

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

    /// Creates a client instance and returns its record.
    fn create(&self, name: &str, minecraft: &str) -> serde_json::Value {
        self.json(&[
            "--json",
            "instance",
            "create",
            name,
            "--minecraft",
            minecraft,
        ])
    }
}

/// `<root>/minecraft/options.txt` of an instance record.
fn options_path(record: &serde_json::Value) -> PathBuf {
    PathBuf::from(record["root"].as_str().expect("root"))
        .join("minecraft")
        .join("options.txt")
}

fn options_of(record: &serde_json::Value) -> String {
    std::fs::read_to_string(options_path(record)).expect("an options file")
}

#[test]
fn one_global_preference_reaches_two_minecraft_versions_without_copying_a_file() {
    // The first required behavior, end to end.
    let sandbox = Sandbox::new();
    sandbox.json(&["--json", "preferences", "set", "gui_scale", "3"]);

    let legacy = sandbox.create("Legacy repro", "1.12.2");
    let modern = sandbox.create("Modern repro", "1.20.1");

    let legacy_options = options_of(&legacy);
    let modern_options = options_of(&modern);
    assert!(legacy_options.contains("guiScale:3"), "{legacy_options}");
    assert!(modern_options.contains("guiScale:3"), "{modern_options}");

    // Each file is marked with its own release's data version, which is what
    // tells the game how much of its own history to apply to the rest of the
    // file. Neither was copied from the other: the markers differ, and 1.12.2
    // has none to give because its client jar never carried one.
    assert!(
        modern_options.contains("version:3465"),
        "1.20.1 marks its own file: {modern_options}"
    );
    assert!(
        !legacy_options.contains("version:"),
        "1.12.2 has no data version to claim: {legacy_options}"
    );

    // Each release resolves to its own schema family, and the report says
    // which one and where the knowledge came from.
    let legacy_schema = legacy["preferences"]["schema"].as_str().expect("a schema");
    let modern_schema = modern["preferences"]["schema"].as_str().expect("a schema");
    assert_ne!(
        legacy_schema, modern_schema,
        "two eras this far apart do not share one representation"
    );
    assert_eq!(legacy["preferences"]["source"], "bundled");
    assert!(
        legacy["preferences"]["created_file"]
            .as_bool()
            .expect("bool")
    );
}

#[test]
fn editing_one_instance_mutates_neither_the_settings_nor_another_instance() {
    // The second required behavior: after creation, the
    // instance owns its options file, and nothing reaches back.
    let sandbox = Sandbox::new();
    sandbox.json(&["--json", "preferences", "set", "gui_scale", "2"]);

    let first = sandbox.create("First", "1.20.1");
    let second = sandbox.create("Second", "1.20.1");

    // Stand in for the user changing the setting inside the game.
    std::fs::write(options_path(&first), "guiScale:4\nrenderDistance:32\n").expect("write");

    assert!(options_of(&second).contains("guiScale:2"));

    let preferences = sandbox.json(&["--json", "preferences", "show"]);
    let gui_scale = preferences["preferences"]
        .as_array()
        .expect("array")
        .iter()
        .find(|entry| entry["preference"] == "gui_scale")
        .expect("gui_scale is listed");
    assert_eq!(gui_scale["value"], "2", "the global setting is untouched");
    assert_eq!(gui_scale["managed"], true);

    // A third instance created afterwards still gets the global value...
    let third = sandbox.create("Third", "1.20.1");
    assert!(options_of(&third).contains("guiScale:2"));
    //...and the edited instance is still exactly as the user left it.
    assert_eq!(
        std::fs::read_to_string(options_path(&first)).expect("read"),
        "guiScale:4\nrenderDistance:32\n"
    );
}

#[test]
fn a_preference_a_release_cannot_express_is_reported_rather_than_written() {
    // Simulation distance arrived with 1.18. Writing it into a 1.12.2 instance
    // would add a key that release never reads, and the static analysis proves
    // the absence rather than inferring it, so it is reported as unsupported
    // and not as unknown.
    let sandbox = Sandbox::new();
    sandbox.json(&["--json", "preferences", "set", "simulation_distance", "10"]);

    let legacy = sandbox.create("Legacy", "1.12.2");
    let modern = sandbox.create("Modern", "1.20.1");

    assert!(
        !options_path(&legacy).exists(),
        "a release with no applicable overrides needs no launcher-written options file"
    );
    assert!(options_of(&modern).contains("simulationDistance:10"));

    let unsupported = legacy["preferences"]["unsupported"]
        .as_array()
        .expect("array");
    assert_eq!(unsupported.len(), 1);
    assert_eq!(unsupported[0], "simulation_distance");
    assert!(
        legacy["preferences"]["unknown"]
            .as_array()
            .expect("array")
            .is_empty(),
        "a proven absence is not the same answer as an unanswered question"
    );
    assert!(
        modern["preferences"]["unsupported"]
            .as_array()
            .expect("array")
            .is_empty()
    );
}

#[test]
fn a_bad_preference_value_is_refused_with_the_syntax_that_would_have_worked() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["preferences", "set", "gui_scale", "enormous"]);
    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("auto, or 1 to 4"), "stderr was:\n{stderr}");
    assert!(
        !stderr.contains("{$"),
        "the localized action lost an argument:\n{stderr}"
    );

    //...and nothing was stored, so instances created afterwards are unaffected.
    let record = sandbox.create("Unaffected", "1.20.1");
    assert!(
        !options_path(&record).exists(),
        "a rejected override must not create an options file"
    );
}

#[test]
fn unmanaging_a_preference_stops_applying_it_and_keeps_its_value() {
    let sandbox = Sandbox::new();
    sandbox.json(&["--json", "preferences", "set", "render_distance", "16"]);
    assert!(options_of(&sandbox.create("Managed", "1.20.1")).contains("renderDistance:16"));

    sandbox.json(&["--json", "preferences", "unmanage", "render_distance"]);
    let after = sandbox.create("Unmanaged", "1.20.1");
    assert!(
        !options_path(&after).exists(),
        "an unmanaged-only configuration must leave options.txt to Minecraft"
    );

    let shown = sandbox.json(&["--json", "preferences", "show"]);
    let entry = shown["preferences"]
        .as_array()
        .expect("array")
        .iter()
        .find(|entry| entry["preference"] == "render_distance")
        .expect("listed");
    assert_eq!(entry["managed"], false);
    assert_eq!(entry["value"], "16", "the value is remembered");
}

#[test]
fn duplicating_an_instance_copies_its_files_without_re_applying_preferences() {
    // the required preference application order.
    let sandbox = Sandbox::new();
    sandbox.json(&["--json", "preferences", "set", "gui_scale", "2"]);
    let original = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Original",
        "--minecraft",
        "1.20.1",
        "--persistent",
    ]);
    let id = original["id"].as_str().expect("id");

    // Something the user did inside the instance, which the copy must keep.
    std::fs::write(options_path(&original), "guiScale:4\n").expect("write");
    let mods = PathBuf::from(original["root"].as_str().expect("root"))
        .join("minecraft")
        .join("mods");
    std::fs::create_dir_all(&mods).expect("mkdir");
    std::fs::write(mods.join("example.jar"), "jar").expect("write");

    let copy = sandbox.json(&["--json", "instance", "duplicate", id, "--temporary"]);

    assert_ne!(copy["id"], original["id"]);
    assert_eq!(copy["name"], "Original (copy)");
    assert_eq!(copy["storage"], "temporary");
    assert_eq!(copy["minecraft_version"], original["minecraft_version"]);
    assert_eq!(
        copy["skipped_links"],
        serde_json::json!([]),
        "a copy that left nothing out says so"
    );
    assert_eq!(
        options_of(&copy),
        "guiScale:4\n",
        "the copy keeps the options file the original had"
    );
    assert!(
        PathBuf::from(copy["root"].as_str().expect("root"))
            .join("minecraft/mods/example.jar")
            .is_file()
    );

    // The original is untouched and both are indexed.
    assert_eq!(
        std::fs::read_to_string(options_path(&original)).expect("read"),
        "guiScale:4\n"
    );
    assert_eq!(
        sandbox
            .json(&["--json", "instances", "list"])
            .as_array()
            .expect("array")
            .len(),
        2
    );
}

#[cfg(unix)]
#[test]
fn duplicating_an_instance_with_json_reports_the_symlinked_folders_it_did_not_copy() {
    let sandbox = Sandbox::new();
    let original = sandbox.create("Linked", "1.20.1");
    let id = original["id"].as_str().expect("id");
    let root = PathBuf::from(original["root"].as_str().expect("root"));

    let outside = sandbox.base().join("shared-saves");
    std::fs::create_dir_all(&outside).expect("mkdir");
    let minecraft = root.join("minecraft");
    std::fs::create_dir_all(&minecraft).expect("mkdir");
    std::os::unix::fs::symlink(&outside, minecraft.join("saves")).expect("symlink");

    let copy = sandbox.json(&["--json", "instance", "duplicate", id, "--temporary"]);

    assert_eq!(
        copy["skipped_links"],
        serde_json::json!(["minecraft/saves"])
    );
    let copied_root = PathBuf::from(copy["root"].as_str().expect("root"));
    assert!(
        !copied_root.join("minecraft/saves").exists(),
        "the link target was not followed into the copy"
    );
}

#[test]
fn open_folder_actions_create_the_conventional_folders_and_refuse_the_rest() {
    // the launch safety boundary. `--print` is used throughout: a test must not hand a folder
    // to whatever file manager the machine running CI happens to have.
    let sandbox = Sandbox::new();
    let record = sandbox.create("Repro", "1.20.1");
    let id = record["id"].as_str().expect("id");
    let root = PathBuf::from(record["root"].as_str().expect("root"));

    let mods = sandbox.json(&[
        "--json", "instance", "open", id, "--folder", "mods", "--print",
    ]);
    assert_eq!(
        PathBuf::from(mods["path"].as_str().expect("path")),
        root.join("minecraft/mods")
    );
    assert!(root.join("minecraft/mods").is_dir(), "created on demand");

    // An empty crash-reports folder would claim the instance had crashed.
    let refused = sandbox.run(&[
        "instance",
        "open",
        id,
        "--folder",
        "crash-reports",
        "--print",
    ]);
    assert!(!refused.status.success());
    assert!(!root.join("minecraft/crash-reports").exists());

    // An unknown folder name lists the ones that exist rather than guessing.
    let unknown = sandbox.run(&["instance", "open", id, "--folder", "mds", "--print"]);
    assert!(!unknown.status.success());
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(stderr.contains("resourcepacks"), "stderr was:\n{stderr}");
}

#[test]
fn a_refusal_says_its_piece_once() {
    // A failure that is not a `CoreError` has no localized message, so its
    // summary *is* its first technical line. Printing the chain unfiltered
    // said the whole sentence twice.
    let sandbox = Sandbox::new();
    let record = sandbox.create("Repro", "1.20.1");
    let id = record["id"].as_str().expect("id");

    let refused = sandbox.run(&[
        "instance",
        "open",
        id,
        "--folder",
        "crash-reports",
        "--print",
    ]);
    assert!(!refused.status.success());

    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(
        stderr.matches("does not exist yet").count(),
        1,
        "stderr was:\n{stderr}"
    );
    // The wording serves servers as well as client instances, so it names
    // neither.
    assert!(
        !stderr.contains("for this instance"),
        "stderr was:\n{stderr}"
    );
}

#[test]
fn ordinary_commands_do_not_narrate_the_localization_loader() {
    // `i18n_embed` logs "Selecting translations for domain" at INFO once per
    // invocation. It says nothing a user or a bug report needs, and it buried
    // the logs that do matter.
    let sandbox = Sandbox::new();
    let output = binary_sandbox::ephervex_command()
        .args(["instances", "list"])
        .env("HOME", sandbox.base())
        .env("XDG_CONFIG_HOME", sandbox.base().join("config"))
        .env("XDG_DATA_HOME", sandbox.base().join("data"))
        .env("XDG_CACHE_HOME", sandbox.base().join("cache"))
        .env("XDG_STATE_HOME", sandbox.base().join("state"))
        .env("TMPDIR", sandbox.base().join("tmp"))
        // Deliberately *not* setting EPHERVEX_LOG: this is about the default.
        .env_remove("EPHERVEX_LOG")
        .output()
        .expect("run the ephervex binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Selecting translations"),
        "stderr was:\n{stderr}"
    );
}

#[test]
fn server_folder_actions_validate_names_before_lookup() {
    let sandbox = Sandbox::new();
    let id = "00000000-0000-0000-0000-000000000001";

    let supported = sandbox.run(&["server", "open", id, "--folder", "config", "--print"]);
    assert!(!supported.status.success());
    assert!(
        String::from_utf8_lossy(&supported.stderr).contains("no server with id"),
        "a supported folder must reach server lookup; stderr was:\n{}",
        String::from_utf8_lossy(&supported.stderr)
    );

    let unsupported = sandbox.run(&["server", "open", id, "--folder", "saves", "--print"]);
    assert!(!unsupported.status.success());
    assert!(
        String::from_utf8_lossy(&unsupported.stderr).contains("invalid value"),
        "a client-only folder must be rejected during parsing"
    );
}

#[test]
fn the_persistent_instance_root_can_be_moved_and_new_instances_follow_it() {
    // the one relocatable root.
    let sandbox = Sandbox::new();
    let elsewhere = sandbox.base().join("elsewhere");

    let before = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Before",
        "--minecraft",
        "1.20.1",
        "--persistent",
    ]);

    let moved = sandbox.json(&[
        "--json",
        "config",
        "instance-root",
        elsewhere.to_str().expect("utf-8"),
    ]);
    assert_eq!(moved["overridden"], true);
    assert_eq!(
        PathBuf::from(moved["persistent_instance_root"].as_str().expect("path")),
        elsewhere
    );

    let after = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "After",
        "--minecraft",
        "1.20.1",
        "--persistent",
    ]);
    assert!(
        PathBuf::from(after["root"].as_str().expect("root")).starts_with(&elsewhere),
        "{after}"
    );

    // The instance created before the move stays where it is, keeps working,
    // and is still listed.
    let before_root = PathBuf::from(before["root"].as_str().expect("root"));
    assert!(before_root.is_dir());
    assert!(!before_root.starts_with(&elsewhere));
    assert_eq!(
        sandbox
            .json(&["--json", "instances", "list"])
            .as_array()
            .expect("array")
            .len(),
        2
    );

    // Temporary instances are unaffected: only this root moves.
    let temporary = sandbox.create("Temporary", "1.20.1");
    assert!(
        PathBuf::from(temporary["root"].as_str().expect("root"))
            .starts_with(sandbox.base().join("tmp"))
    );
}

#[test]
fn preferences_are_stored_in_the_configuration_file_and_survive_a_restart() {
    let sandbox = Sandbox::new();
    sandbox.json(&["--json", "preferences", "set", "language", "de_de"]);

    let config = sandbox.run(&["config"]);
    assert!(config.status.success());
    let text = String::from_utf8_lossy(&config.stdout);
    assert!(text.contains("[minecraft_preferences.language]"), "{text}");
    assert!(text.contains("de_de"), "{text}");

    // A separate process reads it back, which is the whole point of storing it.
    // Creation behavior with no cached language catalog is covered in core;
    // this persistence test needs no production metadata request.
    let shown = sandbox.json(&["--json", "preferences", "show"]);
    let language = shown["preferences"]
        .as_array()
        .expect("array")
        .iter()
        .find(|entry| entry["preference"] == "language")
        .expect("language listed");
    assert_eq!(language["managed"], true);
    assert_eq!(language["value"], "de_de");
}
