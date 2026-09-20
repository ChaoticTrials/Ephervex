//! Dependency-aware isolation uses one graph and preserves complete closures.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use ephervex_core::{
    App, InstalledHost, IsolationConfidence, IsolationRequest, IsolationStep, Paths, RoundAnswer,
};
use ephervex_model::LoaderSelection;
use ephervex_storage::{LaunchLock, NewClient, NewServer, Registry};
use serde_json::json;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

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

fn request(pinned: &[&str]) -> IsolationRequest {
    IsolationRequest {
        pinned: mods(pinned),
        accept_unrecorded: false,
    }
}

fn enabled_files(fixture: &Fixture) -> Vec<String> {
    let mut names = std::fs::read_dir(fixture.root.join("minecraft/mods"))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with(".jar"))
        .collect::<Vec<_>>();
    names.sort();
    names
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

#[test]
fn ddmin_progression_is_deterministic_and_preserves_dependency_closures() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let cancel = CancellationToken::new();
    let first = app
        .begin_isolation(fixture.host, request(&["pin"]), &cancel)
        .unwrap();
    assert_eq!(first.enabled_roots, mods(&["a", "b", "c"]));
    assert_eq!(
        first.enabled,
        mods(&["a", "b", "base", "c", "pin", "shared"])
    );
    assert_eq!(
        enabled_files(&fixture),
        [
            "a.jar",
            "b.jar",
            "base.jar",
            "c.jar",
            "pin.jar",
            "shared.jar"
        ]
    );

    let IsolationStep::AwaitingAnswer(second) = app
        .answer_isolation_round(fixture.host, RoundAnswer::DidNotReproduce)
        .unwrap()
    else {
        panic!("second round");
    };
    assert_eq!(second.enabled_roots, mods(&["d", "e"]));
    assert_eq!(second.enabled, mods(&["base", "d", "e", "pin"]));

    let IsolationStep::AwaitingAnswer(third) = app
        .answer_isolation_round(fixture.host, RoundAnswer::Reproduced)
        .unwrap()
    else {
        panic!("third round");
    };
    assert_eq!(third.enabled_roots, mods(&["d"]));

    let IsolationStep::AwaitingAnswer(fourth) = app
        .answer_isolation_round(fixture.host, RoundAnswer::DidNotReproduce)
        .unwrap()
    else {
        panic!("fourth round");
    };
    assert_eq!(fourth.enabled_roots, mods(&["e"]));
    let IsolationStep::Complete(outcome) = app
        .answer_isolation_round(fixture.host, RoundAnswer::Reproduced)
        .unwrap()
    else {
        panic!("completed search");
    };
    assert_eq!(outcome.roots, mods(&["e"]));
    assert_eq!(outcome.confidence, IsolationConfidence::SmallestReached);
    assert_eq!(enabled_files(&fixture), ["base.jar", "e.jar", "pin.jar"]);
}

#[test]
fn ddmin_grows_granularity_for_an_interacting_pair() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let cancel = CancellationToken::new();
    let mut round = app
        .begin_isolation(fixture.host, request(&["pin"]), &cancel)
        .unwrap();
    let mut saw_four_way_split = false;
    loop {
        let reproduces = round.enabled_roots.contains(&mods(&["a"])[0])
            && round.enabled_roots.contains(&mods(&["e"])[0]);
        match app
            .answer_isolation_round(
                fixture.host,
                if reproduces {
                    RoundAnswer::Reproduced
                } else {
                    RoundAnswer::DidNotReproduce
                },
            )
            .unwrap()
        {
            IsolationStep::AwaitingAnswer(next) => {
                saw_four_way_split |= next.granularity == 4;
                round = next;
            }
            IsolationStep::Complete(outcome) => {
                assert_eq!(outcome.roots, mods(&["a", "e"]));
                assert_eq!(outcome.confidence, IsolationConfidence::OneMinimal);
                break;
            }
            IsolationStep::Contradiction(_) => panic!("consistent oracle contradicted itself"),
        }
    }
    assert!(saw_four_way_split);
}

#[test]
fn reopening_and_resuming_reapplies_the_persisted_round() {
    let fixture = Fixture::new();
    let cancel = CancellationToken::new();
    let expected = fixture
        .app()
        .begin_isolation(fixture.host, request(&["pin"]), &cancel)
        .unwrap();
    std::fs::rename(
        fixture.root.join("minecraft/mods/d.jar.disabled"),
        fixture.root.join("minecraft/mods/d.jar"),
    )
    .unwrap();

    let reopened = fixture.app();
    let state = reopened.isolation_state(fixture.host).unwrap().unwrap();
    assert_eq!(state.current_round.as_ref(), Some(&expected));
    assert!(matches!(
        reopened.resume_isolation(fixture.host).unwrap(),
        IsolationStep::AwaitingAnswer(round) if round == expected
    ));
    assert!(!fixture.root.join("minecraft/mods/d.jar").exists());
}

#[test]
fn undo_restores_the_exact_round_and_configuration() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let cancel = CancellationToken::new();
    let first = app
        .begin_isolation(fixture.host, request(&["pin"]), &cancel)
        .unwrap();
    app.answer_isolation_round(fixture.host, RoundAnswer::DidNotReproduce)
        .unwrap();
    let restored = app.undo_isolation_round(fixture.host).unwrap();
    assert_eq!(restored, first);
    assert_eq!(
        enabled_files(&fixture),
        [
            "a.jar",
            "b.jar",
            "base.jar",
            "c.jar",
            "pin.jar",
            "shared.jar"
        ]
    );
}

#[test]
fn cancel_restores_the_exact_pre_isolation_mod_state_through_the_snapshot() {
    let fixture = Fixture::new();
    std::fs::rename(
        fixture.root.join("minecraft/mods/c.jar"),
        fixture.root.join("minecraft/mods/c.jar.disabled"),
    )
    .unwrap();
    let before = std::fs::read_dir(fixture.root.join("minecraft/mods"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect::<std::collections::BTreeSet<_>>();
    let app = fixture.app();
    let cancel = CancellationToken::new();
    app.begin_isolation(fixture.host, request(&["pin"]), &cancel)
        .unwrap();
    app.answer_isolation_round(fixture.host, RoundAnswer::DidNotReproduce)
        .unwrap();
    app.cancel_isolation(fixture.host, &cancel).unwrap();
    let after = std::fs::read_dir(fixture.root.join("minecraft/mods"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(after, before);
    assert!(!fixture.root.join(".launcher/isolation.json").exists());
}

#[test]
fn finish_keeps_the_minimized_configuration_and_safety_snapshot() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let cancel = CancellationToken::new();
    app.begin_isolation(fixture.host, request(&["pin"]), &cancel)
        .unwrap();
    app.answer_isolation_round(fixture.host, RoundAnswer::DidNotReproduce)
        .unwrap();
    app.answer_isolation_round(fixture.host, RoundAnswer::Reproduced)
        .unwrap();
    app.answer_isolation_round(fixture.host, RoundAnswer::DidNotReproduce)
        .unwrap();
    app.answer_isolation_round(fixture.host, RoundAnswer::Reproduced)
        .unwrap();
    let report = app.finish_isolation(fixture.host).unwrap();
    assert_eq!(report.reproducing[0].root, mods(&["e"])[0]);
    assert_eq!(enabled_files(&fixture), ["base.jar", "e.jar", "pin.jar"]);
    assert_eq!(app.snapshots(fixture.host).unwrap().len(), 1);
    assert!(!fixture.root.join(".launcher/isolation.json").exists());
}

#[test]
fn round_mutations_refuse_a_running_host_but_the_session_does_not_hold_the_lock() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let cancel = CancellationToken::new();
    app.begin_isolation(fixture.host, request(&["pin"]), &cancel)
        .unwrap();
    let _launch = LaunchLock::acquire(&fixture.root).unwrap();
    assert!(app.isolation_state(fixture.host).unwrap().is_some());
    assert!(app.resume_isolation(fixture.host).is_err());
    assert!(
        app.answer_isolation_round(fixture.host, RoundAnswer::Reproduced)
            .is_err()
    );
    assert!(app.undo_isolation_round(fixture.host).is_err());
    assert!(app.finish_isolation(fixture.host).is_err());
    assert!(app.cancel_isolation(fixture.host, &cancel).is_err());
}

#[test]
fn unrecorded_knowledge_requires_an_explicit_reduced_confidence_override() {
    let fixture = Fixture::new();
    fixture.recipe(|recipe| {
        recipe["files"][0]
            .as_object_mut()
            .unwrap()
            .remove("relations");
    });
    let app = fixture.app();
    let cancel = CancellationToken::new();
    assert!(
        app.begin_isolation(fixture.host, request(&[]), &cancel)
            .is_err()
    );
    assert!(app.snapshots(fixture.host).unwrap().is_empty());
    let round = app
        .begin_isolation(
            fixture.host,
            IsolationRequest {
                pinned: Vec::new(),
                accept_unrecorded: true,
            },
            &cancel,
        )
        .unwrap();
    assert!(!round.enabled.is_empty());
    let state = app.isolation_state(fixture.host).unwrap().unwrap();
    assert!(state.accepted_unrecorded);
    assert!(state.reduced_confidence);
}

#[test]
fn unidentifiable_mods_continue_as_candidates_with_a_caveat() {
    let fixture = Fixture::new();
    fixture.write("minecraft/mods/handplaced.jar", b"unknown");
    let app = fixture.app();
    let cancel = CancellationToken::new();
    app.begin_isolation(fixture.host, request(&[]), &cancel)
        .unwrap();
    let state = app.isolation_state(fixture.host).unwrap().unwrap();
    assert_eq!(state.plan.unidentifiable, mods(&["handplaced"]));
    assert!(state.reduced_confidence);
    assert!(
        state
            .plan
            .candidates
            .iter()
            .any(|candidate| candidate.root == mods(&["handplaced"])[0])
    );
}

#[test]
fn missing_dependencies_block_start_before_a_snapshot_or_mutation() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.root.join("minecraft/mods/base.jar")).unwrap();
    let before = enabled_files(&fixture);
    let app = fixture.app();
    assert!(
        app.begin_isolation(fixture.host, request(&[]), &CancellationToken::new())
            .is_err()
    );
    assert_eq!(enabled_files(&fixture), before);
    assert!(app.snapshots(fixture.host).unwrap().is_empty());
    assert!(!fixture.root.join(".launcher/isolation.json").exists());
}

#[test]
fn contradictory_superset_answer_stops_until_undo() {
    let fixture = Fixture::new();
    let app = fixture.app();
    app.begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    let path = fixture.root.join(".launcher/isolation.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut prior = document["current_round"].clone();
    prior["answer"] = json!("reproduced");
    prior["answered_at"] = json!("2026-09-20T00:00:00Z");
    prior["search_before"] = document["search"].clone();
    document["history"] = json!([prior]);
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();

    let IsolationStep::Contradiction(contradiction) = app
        .answer_isolation_round(fixture.host, RoundAnswer::DidNotReproduce)
        .unwrap()
    else {
        panic!("contradiction");
    };
    assert_eq!(contradiction.reproducing_round, 1);
    assert!(
        app.isolation_state(fixture.host)
            .unwrap()
            .unwrap()
            .current_round
            .is_none()
    );
    assert_eq!(app.undo_isolation_round(fixture.host).unwrap().index, 1);
}

#[test]
fn client_isolation_reports_but_never_mutates_a_linked_server() {
    let fixture = Fixture::new();
    let registry = Registry::open(fixture.paths.clone()).unwrap();
    let client_id = match fixture.host {
        InstalledHost::Instance(id) => id,
        InstalledHost::Server(_) => unreachable!(),
    };
    let server = registry
        .create_server(NewServer::linked("Linked", client_id))
        .unwrap();
    let server_mods = server.root.join("server/mods");
    std::fs::create_dir_all(&server_mods).unwrap();
    std::fs::write(server_mods.join("sentinel.jar"), b"server").unwrap();

    let app = fixture.app();
    app.begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    let state = app.isolation_state(fixture.host).unwrap().unwrap();
    assert_eq!(state.plan.linked_servers.len(), 1);
    assert_eq!(state.plan.linked_servers[0].name, "Linked");
    assert_eq!(
        std::fs::read(server_mods.join("sentinel.jar")).unwrap(),
        b"server"
    );
    assert!(!server_mods.join("sentinel.jar.disabled").exists());
}
