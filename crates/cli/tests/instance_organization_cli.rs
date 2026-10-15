//! Searching, filtering, ordering, favorites, groups and icons, driven through
//! the `ephervex` binary.
//!
//! Every run is pointed at a throwaway directory through the XDG variables and
//! `TMPDIR`, and nothing here needs the network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Output;

use tempfile::TempDir;

mod binary_sandbox;
mod support;

/// A valid 1x1 PNG.
const PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xfc, 0xcf, 0xc0, 0x50,
    0x0f, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xa9, 0x8c, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

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

    fn ok(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "`ephervex {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.ok(args);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "`ephervex {}` did not print JSON ({error}):\n{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn create(&self, name: &str, minecraft: &str) -> String {
        self.json(&[
            "--json",
            "instance",
            "create",
            name,
            "--minecraft",
            minecraft,
        ])["id"]
            .as_str()
            .expect("id")
            .to_owned()
    }

    fn listed_names(&self, extra: &[&str]) -> Vec<String> {
        let mut args = vec!["--json", "instances", "list"];
        args.extend_from_slice(extra);
        self.json(&args)
            .as_array()
            .expect("an array")
            .iter()
            .map(|row| row["name"].as_str().expect("name").to_owned())
            .collect()
    }

    fn png(&self, name: &str) -> PathBuf {
        let path = self.base().join(name);
        std::fs::write(&path, PNG).expect("write png");
        path
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_new_instance_lists_without_organization_and_reports_it_in_json() {
    let sandbox = Sandbox::new();
    sandbox.create("Alpha", "1.20.1");
    let listed = sandbox.json(&["--json", "instances", "list"]);
    let row = &listed[0];
    assert_eq!(row["favorite"], false);
    assert_eq!(row["linking"], "none");
    assert!(row["group"].is_null());
    assert!(row["icon"].is_null());
}

#[test]
fn favorites_list_first_and_the_favorite_filter_keeps_only_them() {
    let sandbox = Sandbox::new();
    sandbox.create("Alpha", "1.20.1");
    let beta = sandbox.create("Beta", "1.20.1");
    assert_eq!(sandbox.listed_names(&[]), ["Beta", "Alpha"], "newest first");

    let marked = sandbox.json(&["--json", "instance", "favorite", &beta]);
    assert_eq!(marked["favorite"], true);
    sandbox.create("Gamma", "1.20.1");
    assert_eq!(sandbox.listed_names(&[]), ["Beta", "Gamma", "Alpha"]);
    assert_eq!(sandbox.listed_names(&["--favorite"]), ["Beta"]);

    let cleared = sandbox.json(&["--json", "instance", "unfavorite", &beta]);
    assert_eq!(cleared["favorite"], false);
    assert_eq!(sandbox.listed_names(&["--favorite"]), Vec::<String>::new());
}

#[test]
fn search_sorting_and_version_filters_combine() {
    let sandbox = Sandbox::new();
    sandbox.create("zeta repro", "1.20.1");
    sandbox.create("Alpha repro", "1.12.2");
    sandbox.create("Other", "1.20.1");

    assert_eq!(
        sandbox.listed_names(&["--search", "REPRO", "--sort", "name"]),
        ["Alpha repro", "zeta repro"]
    );
    assert_eq!(
        sandbox.listed_names(&["--search", "repro", "--minecraft", "1.20.1"]),
        ["zeta repro"]
    );
    assert_eq!(
        sandbox.listed_names(&["--sort", "minecraft", "--search", "repro"]),
        ["zeta repro", "Alpha repro"],
        "newest release first"
    );
    assert_eq!(
        sandbox.listed_names(&["--loader", "forge"]),
        Vec::<String>::new()
    );
    assert_eq!(
        sandbox
            .listed_names(&["--stopped", "--loader", "vanilla"])
            .len(),
        3
    );
    assert_eq!(sandbox.listed_names(&["--running"]), Vec::<String>::new());
    assert_eq!(sandbox.listed_names(&["--linking", "none"]).len(), 3);
    assert_eq!(
        sandbox.listed_names(&["--linking", "paired"]),
        Vec::<String>::new()
    );
}

#[test]
fn the_interface_sort_setting_is_the_default_order() {
    let sandbox = Sandbox::new();
    sandbox.create("Alpha", "1.20.1");
    sandbox.create("beta", "1.20.1");
    let config = sandbox.base().join("config").join("ephervex");
    std::fs::create_dir_all(&config).expect("dir");
    std::fs::write(
        config.join("config.toml"),
        "schema_version = 1\n[display]\ninstance_sort = \"name\"\n",
    )
    .expect("write");
    assert_eq!(sandbox.listed_names(&[]), ["Alpha", "beta"]);
    assert_eq!(
        sandbox.listed_names(&["--sort", "favorite"]),
        ["beta", "Alpha"],
        "newest first when nothing is a favorite"
    );
}

#[test]
fn groups_are_created_assigned_renamed_and_deleted_without_touching_the_instance() {
    let sandbox = Sandbox::new();
    let alpha = sandbox.create("Alpha", "1.20.1");
    let before = sandbox.json(&["--json", "instance", "show", &alpha]);

    sandbox.ok(&["groups", "create", "Regression"]);
    let duplicate = sandbox.run(&["groups", "create", "regression"]);
    assert!(!duplicate.status.success());

    let assigned = sandbox.json(&["--json", "instance", "group", &alpha, "REGRESSION"]);
    assert_eq!(assigned["group"]["name"], "Regression");
    assert_eq!(assigned["id"], before["id"]);
    assert_eq!(assigned["root"], before["root"]);
    assert_eq!(sandbox.listed_names(&["--group", "regression"]), ["Alpha"]);
    assert_eq!(sandbox.listed_names(&["--ungrouped"]), Vec::<String>::new());
    assert_eq!(
        sandbox.listed_names(&["--search", "regress"]),
        ["Alpha"],
        "search covers the group name"
    );

    sandbox.ok(&["groups", "rename", "Regression", "Bisect"]);
    let listed = sandbox.json(&["--json", "groups", "list"]);
    assert_eq!(listed[0]["name"], "Bisect");
    assert_eq!(listed[0]["instances"], 1);
    assert_eq!(sandbox.listed_names(&["--group", "Bisect"]), ["Alpha"]);

    let unknown = sandbox.run(&["instance", "group", &alpha, "Missing"]);
    assert!(!unknown.status.success());

    let deleted = sandbox.ok(&["groups", "delete", "Bisect"]);
    assert!(
        stderr(&deleted).contains("ungrouped"),
        "{}",
        stderr(&deleted)
    );
    let after = sandbox.json(&["--json", "instance", "show", &alpha]);
    assert!(after["group"].is_null());
    assert_eq!(after["id"], before["id"]);
}

#[test]
fn an_instance_can_be_taken_out_of_its_group() {
    let sandbox = Sandbox::new();
    let alpha = sandbox.create("Alpha", "1.20.1");
    sandbox.ok(&["groups", "create", "G"]);
    sandbox.ok(&["instance", "group", &alpha, "G"]);
    let cleared = sandbox.json(&["--json", "instance", "group", &alpha, "--none"]);
    assert!(cleared["group"].is_null());
    let text = String::from_utf8_lossy(&sandbox.ok(&["groups", "list"]).stdout).into_owned();
    assert!(text.contains("0 instances"), "{text}");
}

#[test]
fn an_icon_is_stored_once_cleared_and_a_non_image_is_refused() {
    let sandbox = Sandbox::new();
    let alpha = sandbox.create("Alpha", "1.20.1");
    let png = sandbox.png("icon.png");

    let set = sandbox.json(&["--json", "instance", "icon", &alpha, png.to_str().unwrap()]);
    let stored = PathBuf::from(set["icon"].as_str().expect("a stored icon"));
    assert!(stored.is_file());
    assert!(stored.starts_with(sandbox.base().join("data")));

    let text = sandbox.base().join("notes.png");
    std::fs::write(&text, b"not an image").expect("write");
    let refused = sandbox.run(&["instance", "icon", &alpha, text.to_str().unwrap()]);
    assert!(!refused.status.success());
    assert!(stored.is_file(), "a refused icon keeps the previous one");

    let cleared = sandbox.json(&["--json", "instance", "icon", &alpha, "--clear"]);
    assert!(cleared["icon"].is_null());
    assert!(stored.exists(), "the previous copy still names it");
    sandbox.ok(&["instance", "favorite", &alpha]);
    assert!(!stored.exists(), "an icon nothing names is removed");
}

#[test]
fn server_organization_commands_refuse_an_unknown_server() {
    let sandbox = Sandbox::new();
    let id = "00000000-0000-0000-0000-000000000001";
    let png = sandbox.png("icon.png");
    for args in [
        vec!["server", "favorite", id],
        vec!["server", "unfavorite", id],
        vec!["server", "icon", id, png.to_str().unwrap()],
        vec!["server", "icon", id, "--clear"],
    ] {
        assert!(!sandbox.run(&args).status.success(), "{args:?}");
    }
}

#[test]
fn the_help_describes_the_organization_commands_for_users() {
    let sandbox = Sandbox::new();
    let list =
        String::from_utf8_lossy(&sandbox.ok(&["instances", "list", "--help"]).stdout).into_owned();
    for flag in [
        "--search",
        "--favorite",
        "--group",
        "--ungrouped",
        "--minecraft",
        "--loader",
        "--running",
        "--stopped",
        "--linking",
        "--sort",
    ] {
        assert!(list.contains(flag), "{flag} missing from:\n{list}");
    }
    let top = String::from_utf8_lossy(&sandbox.ok(&["--help"]).stdout).into_owned();
    assert!(top.contains("groups"), "{top}");
    for text in [list, top] {
        assert!(!text.contains("ADR"), "{text}");
    }
}

fn organization_file(sandbox: &Sandbox) -> PathBuf {
    sandbox
        .base()
        .join("data")
        .join("ephervex")
        .join("organization.json")
}

#[test]
fn a_damaged_organization_file_is_set_aside_and_reported_by_whichever_command_finds_it() {
    let sandbox = Sandbox::new();
    sandbox.create("Alpha", "1.20.1");
    sandbox.ok(&["groups", "create", "G"]);
    let file = organization_file(&sandbox);
    assert!(file.is_file(), "{}", file.display());
    std::fs::write(&file, "{ not json").expect("damage");

    let listed = sandbox.ok(&["instances", "list"]);
    assert!(
        stderr(&listed).contains("could not be read"),
        "{}",
        stderr(&listed)
    );
    assert!(!file.exists(), "the damaged file is moved aside");
    let kept = std::fs::read_dir(file.parent().unwrap())
        .expect("list")
        .flatten()
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("organization.unreadable-")
        });
    assert!(kept, "and kept");

    // A change command that finds a damaged file says so as well.
    std::fs::write(&file, "{ not json").expect("damage");
    let changed = sandbox.ok(&["groups", "create", "H"]);
    assert!(
        stderr(&changed).contains("could not be read"),
        "{}",
        stderr(&changed)
    );
}

#[test]
fn a_record_is_still_shown_when_the_organization_cannot_be_read() {
    let sandbox = Sandbox::new();
    let alpha = sandbox.create("Alpha", "1.20.1");
    let file = organization_file(&sandbox);
    std::fs::create_dir_all(&file).expect("a directory where the file belongs");

    let shown = sandbox.json(&["--json", "instance", "show", &alpha]);
    assert_eq!(shown["favorite"], false);
    assert!(shown["group"].is_null());
    sandbox.ok(&["instance", "show", &alpha]);
}

#[test]
fn group_commands_report_how_many_instances_a_group_holds() {
    let sandbox = Sandbox::new();
    let alpha = sandbox.create("Alpha", "1.20.1");
    sandbox.ok(&["groups", "create", "G"]);
    sandbox.ok(&["instance", "group", &alpha, "G"]);

    let renamed = sandbox.json(&["--json", "groups", "rename", "G", "H"]);
    assert_eq!(renamed["instances"], 1);
    let deleted = sandbox.json(&["--json", "groups", "delete", "H"]);
    assert_eq!(deleted["deleted"]["instances"], 1);
}

#[test]
fn a_damaged_organization_file_falls_back_to_its_copy_from_before_the_last_change() {
    let sandbox = Sandbox::new();
    sandbox.ok(&["groups", "create", "Kept"]);
    sandbox.ok(&["groups", "create", "Latest"]);
    std::fs::write(organization_file(&sandbox), "{ not json").expect("damage");

    let listed = sandbox.ok(&["groups", "list"]);
    assert!(
        stderr(&listed).contains("was restored"),
        "{}",
        stderr(&listed)
    );
    assert!(stdout(&listed).contains("Kept"), "{}", stdout(&listed));
    assert!(!stdout(&listed).contains("Latest"), "{}", stdout(&listed));
}

#[test]
fn a_filter_that_matches_nothing_says_so_instead_of_no_instances_yet() {
    let sandbox = Sandbox::new();
    assert!(stdout(&sandbox.ok(&["instances", "list"])).contains("No instances yet."));
    sandbox.create("Alpha", "1.20.1");
    let filtered = sandbox.ok(&["instances", "list", "--favorite"]);
    assert!(
        stdout(&filtered).contains("No instances match."),
        "{}",
        stdout(&filtered)
    );
}
