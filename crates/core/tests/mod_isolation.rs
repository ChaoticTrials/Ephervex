//! Dependency-aware isolation uses one graph and preserves complete closures.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use ephervex_core::{App, InstalledHost, Paths};
use ephervex_model::LoaderSelection;
use ephervex_storage::{NewClient, Registry};
use serde_json::json;
use std::path::{Path, PathBuf};

struct Fixture {
    _directory: tempfile::TempDir,
    paths: Paths,
    root: PathBuf,
    host: InstalledHost,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(directory.path());
        let registry = Registry::open(paths.clone()).unwrap();
        let client = registry
            .create_client(NewClient::new(
                "Isolation",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .unwrap();
        registry.publish_client(&client).unwrap();
        let fixture = Self {
            _directory: directory,
            paths,
            root: client.root,
            host: InstalledHost::Instance(client.id),
        };
        let files = [
            ("a", vec!["shared"]), ("b", vec!["shared"]), ("c", vec![]),
            ("d", vec![]), ("e", vec![]), ("pin", vec!["base"]),
            ("shared", vec!["base"]), ("base", vec![]),
        ].into_iter().map(|(name, requires)| {
            fixture.write(&format!("minecraft/mods/{name}.jar"), name.as_bytes());
            json!({
                "logical_path": format!("mods/{name}.jar"), "hashes": [],
                "downloads": [], "size": name.len(), "client": "required", "server": "required",
                "origin": "user_selected",
                "identity": {"provider": "modrinth", "project_id": name, "version_id": format!("{name}1")},
                "relations": requires.into_iter().map(|name| json!({"kind":"required","target":{"project":{"project_id":name}}})).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>();
        fixture.write(
            ".launcher/source/recipe.json",
            &serde_json::to_vec(&json!({
                "recipe_version":1, "source_kind":"manual", "original_name":"Isolation",
                "minecraft_version":"1.20.1","loader":{"kind":"vanilla","version":null},
                "files":files, "override_layers":[], "provider_metadata":{"kind":"manual"},
            }))
            .unwrap(),
        );
        fixture
    }
    fn app(&self) -> App {
        App::open_at(self.paths.clone()).unwrap()
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    fn recipe(&self, change: impl FnOnce(&mut serde_json::Value)) {
        let path = self.root.join(".launcher/source/recipe.json");
        let mut value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        change(&mut value);
        self.write(
            ".launcher/source/recipe.json",
            &serde_json::to_vec(&value).unwrap(),
        );
    }
}
fn mods(names: &[&str]) -> Vec<PathBuf> {
    names
        .iter()
        .map(|name| Path::new("mods").join(format!("{name}.jar")))
        .collect()
}

#[test]
fn roots_carry_shared_dependencies_and_the_pinned_transitive_closure() {
    let fixture = Fixture::new();
    let plan = fixture
        .app()
        .plan_isolation(fixture.host, &mods(&["pin"]))
        .unwrap();
    assert_eq!(plan.pinned_closure, mods(&["base", "pin"]));
    assert_eq!(
        plan.candidates
            .iter()
            .map(|c| c.root.clone())
            .collect::<Vec<_>>(),
        mods(&["a", "b", "c", "d", "e"])
    );
    assert_eq!(
        plan.enabled_for(&mods(&["a"])),
        mods(&["a", "base", "pin", "shared"])
    );
    assert_eq!(
        plan.enabled_for(&mods(&["b"])),
        mods(&["b", "base", "pin", "shared"])
    );
    assert_eq!(plan.enabled_for(&mods(&["c"])), mods(&["base", "c", "pin"]));
}

#[test]
fn disabled_required_dependencies_are_carried_but_not_independent_suspects() {
    let fixture = Fixture::new();
    std::fs::rename(
        fixture.root.join("minecraft/mods/shared.jar"),
        fixture.root.join("minecraft/mods/shared.jar.disabled"),
    )
    .unwrap();
    let plan = fixture.app().plan_isolation(fixture.host, &[]).unwrap();
    assert!(
        plan.enabled_for(&mods(&["a"]))
            .contains(&mods(&["shared"])[0])
    );
    assert!(
        !plan
            .candidates
            .iter()
            .any(|c| c.root == mods(&["shared"])[0])
    );
}

#[test]
fn missing_required_dependencies_are_reported_before_any_change() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.root.join("minecraft/mods/base.jar")).unwrap();
    let plan = fixture.app().plan_isolation(fixture.host, &[]).unwrap();
    assert!(
        plan.missing
            .iter()
            .any(|m| m.required_by == mods(&["a"])[0])
    );
    assert!(!fixture.root.join(".launcher/isolation.json").exists());
}

#[test]
fn unknown_dependency_knowledge_is_not_reported_as_recorded_independence() {
    let fixture = Fixture::new();
    fixture.recipe(|recipe| {
        recipe["files"][0]
            .as_object_mut()
            .unwrap()
            .remove("relations");
        recipe["files"][1]
            .as_object_mut()
            .unwrap()
            .remove("relations");
        recipe["files"][1]["identity"]
            .as_object_mut()
            .unwrap()
            .remove("version_id");
    });
    fixture.write("minecraft/mods/handplaced.jar", b"unknown");
    let plan = fixture.app().plan_isolation(fixture.host, &[]).unwrap();
    assert_eq!(plan.unrecorded, mods(&["a"]));
    assert_eq!(plan.unidentifiable, mods(&["b", "handplaced"]));
}

#[test]
fn required_cycles_have_one_deterministic_candidate_representative() {
    let fixture = Fixture::new();
    fixture.recipe(|recipe| {
        recipe["files"][2]["relations"] =
            json!([{"kind":"required","target":{"project":{"project_id":"d"}}}]);
        recipe["files"][3]["relations"] =
            json!([{"kind":"required","target":{"project":{"project_id":"c"}}}]);
    });
    let plan = fixture.app().plan_isolation(fixture.host, &[]).unwrap();
    let candidate = plan
        .candidates
        .iter()
        .find(|c| c.root == mods(&["c"])[0])
        .unwrap();
    assert_eq!(candidate.closure, mods(&["c", "d"]));
    assert!(!plan.candidates.iter().any(|c| c.root == mods(&["d"])[0]));
}
