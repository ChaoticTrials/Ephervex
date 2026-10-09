//! Dependency-aware isolation uses one graph and preserves complete closures.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use ephervex_core::{
    App, CoreError, DuplicateClient, InstalledHost, IsolationConfidence, IsolationError,
    IsolationReport, IsolationRequest, IsolationRound, IsolationRoundKind, IsolationStep,
    LinkedModRequest, ModInstallRequest, ModVersionChoice, Paths, RepairRefusal, RoundAnswer,
};
use ephervex_download::{ChannelSink, OperationReporter};
use ephervex_model::{AbsentModPolicy, LoaderSelection};
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
        Self::with_mods(&[
            ("a", &["shared"]),
            ("b", &["shared"]),
            ("c", &[]),
            ("d", &[]),
            ("e", &[]),
            ("pin", &["base"]),
            ("shared", &["base"]),
            ("base", &[]),
        ])
    }

    /// A vanilla client whose recipe declares each named jar and the projects it requires.
    fn with_mods(declared: &[(&str, &[&str])]) -> Self {
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
        let files = declared.iter().map(|&(name, requires)| {
            fixture.write(&format!("minecraft/mods/{name}.jar"), name.as_bytes());
            json!({
                "logical_path": format!("mods/{name}.jar"), "hashes": [],
                "downloads": [], "size": name.len(), "client": "required", "server": "required",
                "origin": "user_selected",
                "identity": {"provider": "modrinth", "project_id": name, "version_id": format!("{name}1")},
                "relations": requires.iter().map(|name| json!({"kind":"required","target":{"project":{"project_id":name}}})).collect::<Vec<_>>(),
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
        accept_missing: false,
    }
}

fn enabled_files(fixture: &Fixture) -> Vec<String> {
    let mut names = std::fs::read_dir(fixture.root.join("minecraft/mods"))
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

/// Answers every round from `reproduces` until the search stops, returning
/// each tested round and the final step.
fn drive(
    app: &App,
    host: InstalledHost,
    first: IsolationRound,
    reproduces: impl Fn(&[PathBuf]) -> bool,
) -> (Vec<IsolationRound>, IsolationStep) {
    let mut rounds = vec![first];
    loop {
        let round = rounds.last().unwrap();
        let answer = if reproduces(&round.enabled) {
            RoundAnswer::Reproduced
        } else {
            RoundAnswer::DidNotReproduce
        };
        match app.answer_isolation_round(host, answer).unwrap() {
            IsolationStep::AwaitingAnswer(next) => {
                assert!(rounds.len() < 64, "the search did not terminate");
                rounds.push(next);
            }
            step => return (rounds, step),
        }
    }
}

fn enables(names: &[&str]) -> impl Fn(&[PathBuf]) -> bool {
    let required = mods(names);
    move |enabled| required.iter().all(|path| enabled.contains(path))
}

fn roots(report: &IsolationReport) -> Vec<PathBuf> {
    report
        .remaining_candidates
        .iter()
        .map(|candidate| candidate.root.clone())
        .collect()
}

fn assert_dependency_closed(fixture: &Fixture, rounds: &[IsolationRound]) {
    let plan = fixture
        .app()
        .isolation_state(fixture.host)
        .unwrap()
        .unwrap()
        .plan;
    for round in rounds {
        assert_eq!(plan.enabled_for(&round.enabled_roots), round.enabled);
        for candidate in &plan.candidates {
            if round.enabled.contains(&candidate.root) {
                assert!(
                    candidate
                        .closure
                        .iter()
                        .all(|path| round.enabled.contains(path)),
                    "round {} enables {} without everything it requires",
                    round.index,
                    candidate.root.display()
                );
            }
        }
    }
}

#[test]
fn every_enabled_mod_outside_the_pinned_closure_is_a_candidate_carrying_its_closure() {
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
        mods(&["a", "b", "c", "d", "e", "shared"])
    );
    assert_eq!(
        plan.enabled_for(&mods(&["a"])),
        mods(&["a", "base", "pin", "shared"])
    );
    assert_eq!(
        plan.enabled_for(&mods(&["b"])),
        mods(&["b", "base", "pin", "shared"])
    );
    assert_eq!(
        plan.enabled_for(&mods(&["shared"])),
        mods(&["base", "pin", "shared"])
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
    let run = || {
        let fixture = Fixture::new();
        let app = fixture.app();
        let first = app
            .begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
            .unwrap();
        let (rounds, step) = drive(&app, fixture.host, first, enables(&["e"]));
        assert_dependency_closed(&fixture, &rounds);
        let IsolationStep::Complete(outcome) = step else {
            panic!("completed search");
        };
        assert_eq!(outcome.roots, mods(&["e"]));
        assert_eq!(outcome.confidence, IsolationConfidence::SmallestReached);
        assert_eq!(enabled_files(&fixture), ["base.jar", "e.jar", "pin.jar"]);
        rounds
            .into_iter()
            .map(|round| round.enabled_roots)
            .collect::<Vec<_>>()
    };
    let rounds = run();
    assert_eq!(
        rounds[0],
        mods(&["a", "b", "c", "shared"]),
        "a subset brings in the suspects its group requires"
    );
    assert_eq!(rounds, run());
}

#[test]
fn starting_reports_planning_snapshot_and_mod_application_stages() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let (sink, mut updates) = ChannelSink::new();
    let reporter = OperationReporter::new("operation-starting-isolation", true, sink);
    reporter.begin(None);

    app.begin_isolation_reporting(
        fixture.host,
        request(&["pin"]),
        &reporter,
        &CancellationToken::new(),
    )
    .unwrap();

    let stages = std::iter::from_fn(|| updates.try_recv().ok())
        .filter_map(|operation| operation.stage)
        .collect::<Vec<_>>();
    assert_eq!(
        stages,
        [
            "operation-stage-planning-isolation",
            "operation-stage-snapshot-isolation",
            "operation-stage-applying-isolation",
        ]
    );
}

#[test]
fn ddmin_grows_granularity_for_an_interacting_pair() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let first = app
        .begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    let (rounds, step) = drive(&app, fixture.host, first, enables(&["a", "e"]));
    assert_dependency_closed(&fixture, &rounds);
    assert!(rounds.iter().any(|round| round.granularity > 2));
    let IsolationStep::Complete(outcome) = step else {
        panic!("consistent oracle contradicted itself");
    };
    assert_eq!(outcome.roots, mods(&["a", "e", "shared"]));
    assert_eq!(outcome.confidence, IsolationConfidence::OneMinimal);
    let report = app.finish_isolation(fixture.host).unwrap();
    assert_eq!(roots(&report), mods(&["a", "e"]));
    assert_eq!(report.required_dependencies, mods(&["shared"]));
}

#[test]
fn a_reproducing_complement_reduces_granularity_by_one() {
    let fixture = Fixture::with_mods(&[("a", &[]), ("b", &[]), ("c", &[]), ("d", &[]), ("e", &[])]);
    let app = fixture.app();
    app.begin_isolation(fixture.host, request(&[]), &CancellationToken::new())
        .unwrap();

    // Both halves failing to reproduce also answers both two-way complements
    // and every four-way subset, so the next question is a four-way complement.
    let mut round = None;
    for _ in 0..2 {
        let IsolationStep::AwaitingAnswer(next) = app
            .answer_isolation_round(fixture.host, RoundAnswer::DidNotReproduce)
            .unwrap()
        else {
            panic!("the search should still be testing");
        };
        round = Some(next);
    }
    let complement = round.expect("a complement round");
    assert_eq!(complement.kind, IsolationRoundKind::Complement);
    assert_eq!(complement.granularity, 4);
    assert_eq!(complement.enabled_roots, mods(&["c", "d", "e"]));

    let IsolationStep::AwaitingAnswer(next) = app
        .answer_isolation_round(fixture.host, RoundAnswer::Reproduced)
        .unwrap()
    else {
        panic!("the reproducing complement should continue narrowing");
    };
    assert_eq!(next.granularity, 3);
    assert_eq!(next.kind, IsolationRoundKind::Complement);
    assert_eq!(next.enabled_roots, mods(&["c", "e"]));
}

#[test]
fn configurations_implied_by_an_earlier_answer_are_not_asked_again() {
    let fixture = Fixture::with_mods(&[("a", &[]), ("b", &[]), ("c", &[]), ("d", &[])]);
    let app = fixture.app();
    let first = app
        .begin_isolation(fixture.host, request(&[]), &CancellationToken::new())
        .unwrap();
    let (rounds, _) = drive(&app, fixture.host, first, enables(&["a", "c"]));
    for (index, round) in rounds.iter().enumerate() {
        for earlier in &rounds[..index] {
            assert_ne!(
                earlier.enabled, round.enabled,
                "a configuration was asked twice"
            );
        }
    }
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

fn interrupt_round(fixture: &Fixture) {
    // What a batch cut short leaves: one jar of the round in the wrong state.
    std::fs::rename(
        fixture.root.join("minecraft/mods/d.jar.disabled"),
        fixture.root.join("minecraft/mods/d.jar"),
    )
    .unwrap();
}

#[test]
fn reconciling_after_a_restart_reapplies_an_interrupted_round() {
    let fixture = Fixture::new();
    let round = fixture
        .app()
        .begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    let applied = enabled_files(&fixture);
    interrupt_round(&fixture);

    let reopened = fixture.app();
    assert_eq!(
        reopened.reconcile_isolation(fixture.host).unwrap(),
        ephervex_core::IsolationReconciliation::Reapplied
    );
    assert_eq!(enabled_files(&fixture), applied);
    assert_eq!(
        reopened
            .isolation_state(fixture.host)
            .unwrap()
            .unwrap()
            .current_round,
        Some(round)
    );
}

#[test]
fn reconciling_a_host_that_already_matches_changes_nothing() {
    let fixture = Fixture::new();
    let app = fixture.app();
    app.begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    let path = fixture.root.join(".launcher/isolation.json");
    let session = std::fs::read(&path).unwrap();
    let applied = enabled_files(&fixture);

    for _ in 0..2 {
        assert_eq!(
            app.reconcile_isolation(fixture.host).unwrap(),
            ephervex_core::IsolationReconciliation::AlreadyApplied
        );
    }
    assert_eq!(enabled_files(&fixture), applied);
    assert_eq!(std::fs::read(&path).unwrap(), session);
}

#[test]
fn an_answer_is_refused_while_the_host_does_not_have_the_rounds_mods() {
    let fixture = Fixture::new();
    let app = fixture.app();
    app.begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    interrupt_round(&fixture);

    assert!(matches!(
        app.answer_isolation_round(fixture.host, RoundAnswer::Reproduced),
        Err(CoreError::Isolation(IsolationError::NotApplied { .. }))
    ));
    let session = app.isolation_state(fixture.host).unwrap().unwrap();
    assert!(session.history.is_empty());

    app.reconcile_isolation(fixture.host).unwrap();
    app.answer_isolation_round(fixture.host, RoundAnswer::Reproduced)
        .unwrap();
}

#[test]
fn a_running_host_is_not_reconciled_until_it_stops() {
    let fixture = Fixture::new();
    let app = fixture.app();
    app.begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    interrupt_round(&fixture);
    let drifted = enabled_files(&fixture);
    let path = fixture.root.join(".launcher/isolation.json");
    let session = std::fs::read(&path).unwrap();

    let launch = LaunchLock::acquire(&fixture.root).unwrap();
    assert!(matches!(
        app.reconcile_isolation(fixture.host),
        Err(CoreError::Isolation(IsolationError::HostRunning { .. }))
    ));
    assert!(
        app.answer_isolation_round(fixture.host, RoundAnswer::Reproduced)
            .is_err()
    );
    assert_eq!(enabled_files(&fixture), drifted);
    assert_eq!(std::fs::read(&path).unwrap(), session);

    drop(launch);
    assert_eq!(
        app.reconcile_isolation(fixture.host).unwrap(),
        ephervex_core::IsolationReconciliation::Reapplied
    );
}

#[test]
fn a_failed_reconciliation_keeps_the_session_and_can_be_retried() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let round = app
        .begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    let path = fixture.root.join(".launcher/isolation.json");
    let session = std::fs::read(&path).unwrap();
    let moved = fixture.root.join("aside.jar");
    std::fs::rename(fixture.root.join("minecraft/mods/a.jar"), &moved).unwrap();

    assert!(app.reconcile_isolation(fixture.host).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), session);
    assert!(
        app.answer_isolation_round(fixture.host, RoundAnswer::Reproduced)
            .is_err()
    );

    std::fs::rename(&moved, fixture.root.join("minecraft/mods/a.jar")).unwrap();
    app.reconcile_isolation(fixture.host).unwrap();
    assert_eq!(
        app.isolation_state(fixture.host)
            .unwrap()
            .unwrap()
            .current_round,
        Some(round)
    );
}

#[test]
fn persisted_sessions_use_compact_version_one_documents_and_reject_newer_versions() {
    let fixture = Fixture::new();
    let app = fixture.app();
    app.begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    let path = fixture.root.join(".launcher/isolation.json");
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.contains(&b'\n'));
    let mut document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(document["isolation_version"], 1);

    document["isolation_version"] = json!(2);
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    assert!(matches!(
        app.isolation_state(fixture.host),
        Err(CoreError::Isolation(IsolationError::UnsupportedVersion {
            found: 2,
            supported: 1,
            ..
        }))
    ));
}

#[test]
fn structurally_incomplete_persisted_sessions_are_rejected() {
    let fixture = Fixture::new();
    let app = fixture.app();
    app.begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    let path = fixture.root.join(".launcher/isolation.json");
    let mut document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    document.as_object_mut().unwrap().remove("current_round");
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();

    assert!(matches!(
        app.isolation_state(fixture.host),
        Err(CoreError::Isolation(IsolationError::Invalid { .. }))
    ));
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
fn a_plan_after_a_snapshot_restore_reads_the_restored_mods() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let cancel = CancellationToken::new();
    let snapshot = app
        .create_snapshot(
            fixture.host,
            "Everything enabled",
            ephervex_core::SnapshotOrigin::Manual,
            &cancel,
        )
        .unwrap();
    app.set_mods_enabled(
        fixture.host,
        &["a", "b", "c", "d"].map(|name| (mods(&[name])[0].clone(), false)),
    )
    .unwrap();
    let narrowed = app.plan_isolation(fixture.host, &[]).unwrap();
    assert_eq!(
        narrowed.initially_enabled,
        mods(&["base", "e", "pin", "shared"])
    );

    app.restore_snapshot(
        fixture.host,
        snapshot.id,
        ephervex_core::SnapshotRestoreOptions {
            safety_snapshot: false,
        },
        &cancel,
    )
    .unwrap();
    let restored = app.plan_isolation(fixture.host, &[]).unwrap();
    assert_eq!(
        restored.initially_enabled,
        mods(&["a", "b", "base", "c", "d", "e", "pin", "shared"])
    );
}

#[test]
fn a_plan_changes_exactly_when_the_mod_state_it_describes_changes() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let cancel = CancellationToken::new();
    let plan = || app.plan_isolation(fixture.host, &[]).unwrap();
    let original = plan();
    let snapshot = app
        .create_snapshot(
            fixture.host,
            "Original",
            ephervex_core::SnapshotOrigin::Manual,
            &cancel,
        )
        .unwrap();

    // Enabling what is already enabled changes nothing on disk.
    app.set_mods_enabled(fixture.host, &[(mods(&["c"])[0].clone(), true)])
        .unwrap();
    assert_eq!(plan(), original);
    assert!(
        app.set_mods_enabled(fixture.host, &[(mods(&["gone"])[0].clone(), false)])
            .is_err()
    );
    assert_eq!(plan(), original, "a refused change is not a change");

    app.set_mods_enabled(fixture.host, &[(mods(&["c"])[0].clone(), false)])
        .unwrap();
    let disabled = plan();
    assert_ne!(disabled, original);
    app.set_mods_enabled(fixture.host, &[(mods(&["c"])[0].clone(), true)])
        .unwrap();
    assert_eq!(plan(), original);

    std::fs::remove_file(fixture.root.join("minecraft/mods/d.jar")).unwrap();
    assert_ne!(plan(), original);
    app.restore_snapshot(
        fixture.host,
        snapshot.id,
        ephervex_core::SnapshotRestoreOptions {
            safety_snapshot: false,
        },
        &cancel,
    )
    .unwrap();
    assert_eq!(plan(), original);

    fixture.recipe(|recipe| {
        recipe["files"][2]["relations"] =
            json!([{"kind":"required","target":{"project":{"project_id":"d"}}}]);
    });
    assert_ne!(
        fixture.app().plan_isolation(fixture.host, &[]).unwrap(),
        original,
        "dependency metadata is part of what a plan describes"
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
    let first = app
        .begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    drive(&app, fixture.host, first, enables(&["e"]));
    let report = app.finish_isolation(fixture.host).unwrap();
    assert_eq!(roots(&report), mods(&["e"]));
    assert_eq!(report.pinned, mods(&["pin"]));
    assert_eq!(report.pinned_dependencies, mods(&["base"]));
    assert!(report.required_dependencies.is_empty());
    assert_eq!(enabled_files(&fixture), ["base.jar", "e.jar", "pin.jar"]);
    assert_eq!(app.snapshots(fixture.host).unwrap().len(), 1);
    assert!(!fixture.root.join(".launcher/isolation.json").exists());
}

/// Removes a session's safety snapshot behind the core's back, as a session
/// written before its deletion was refused would find it.
fn remove_safety_snapshot_directory(app: &App, fixture: &Fixture) {
    let session = app.isolation_state(fixture.host).unwrap().unwrap();
    std::fs::remove_dir_all(
        ephervex_storage::metadata::snapshots_dir(&fixture.root)
            .join(session.safety_snapshot.to_string()),
    )
    .unwrap();
}

#[test]
fn the_safety_snapshot_of_an_active_session_cannot_be_deleted() {
    let fixture = Fixture::new();
    let app = fixture.app();
    app.begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    let snapshot = app
        .isolation_state(fixture.host)
        .unwrap()
        .unwrap()
        .safety_snapshot;

    let error = app.delete_snapshot(fixture.host, snapshot).unwrap_err();

    assert!(matches!(
        error,
        CoreError::Isolation(IsolationError::SafetySnapshotInUse { snapshot: held }) if held == snapshot
    ));
    assert_eq!(app.snapshots(fixture.host).unwrap().len(), 1);
    app.cancel_isolation(fixture.host, &CancellationToken::new())
        .unwrap();
}

#[test]
fn the_safety_snapshot_can_be_deleted_once_the_session_has_finished() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let first = app
        .begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    let snapshot = app
        .isolation_state(fixture.host)
        .unwrap()
        .unwrap()
        .safety_snapshot;
    drive(&app, fixture.host, first, enables(&["e"]));
    app.finish_isolation(fixture.host).unwrap();

    app.delete_snapshot(fixture.host, snapshot).unwrap();

    assert!(app.snapshots(fixture.host).unwrap().is_empty());
}

#[test]
fn another_snapshot_can_be_deleted_during_an_active_session() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let other = app
        .create_snapshot(
            fixture.host,
            "extra",
            ephervex_core::SnapshotOrigin::Manual,
            &CancellationToken::new(),
        )
        .unwrap();
    app.begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();

    app.delete_snapshot(fixture.host, other.id).unwrap();

    assert_eq!(app.snapshots(fixture.host).unwrap().len(), 1);
}

#[test]
fn finish_completes_when_the_safety_snapshot_is_already_gone() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let first = app
        .begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    drive(&app, fixture.host, first, enables(&["e"]));
    remove_safety_snapshot_directory(&app, &fixture);

    let report = app.finish_isolation(fixture.host).unwrap();

    assert_eq!(roots(&report), mods(&["e"]));
    assert_eq!(enabled_files(&fixture), ["base.jar", "e.jar", "pin.jar"]);
    assert!(!fixture.root.join(".launcher/isolation.json").exists());
}

#[test]
fn cancel_ends_the_session_and_re_enables_the_starting_mods_when_the_safety_snapshot_is_gone() {
    let fixture = Fixture::new();
    let before = enabled_files(&fixture);
    let app = fixture.app();
    app.begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    app.answer_isolation_round(fixture.host, RoundAnswer::DidNotReproduce)
        .unwrap();
    assert_ne!(enabled_files(&fixture), before);
    remove_safety_snapshot_directory(&app, &fixture);

    app.cancel_isolation(fixture.host, &CancellationToken::new())
        .unwrap();

    assert_eq!(enabled_files(&fixture), before);
    assert!(!fixture.root.join(".launcher/isolation.json").exists());
}

#[test]
fn finish_separates_remaining_candidates_pins_and_required_dependencies() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let first = app
        .begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    drive(&app, fixture.host, first, enables(&["a"]));
    let report = app.finish_isolation(fixture.host).unwrap();

    assert_eq!(roots(&report), mods(&["a"]));
    assert_eq!(report.pinned, mods(&["pin"]));
    assert_eq!(report.pinned_dependencies, mods(&["base"]));
    assert_eq!(report.required_dependencies, mods(&["shared"]));
    assert!(!report.excluded_roots.contains(&mods(&["shared"])[0]));
    assert!(!report.excluded_roots.contains(&mods(&["base"])[0]));
}

#[test]
fn round_mutations_refuse_a_running_host_but_the_session_does_not_hold_the_lock() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let cancel = CancellationToken::new();
    let launch = LaunchLock::acquire(&fixture.root).unwrap();
    assert!(
        app.begin_isolation(fixture.host, request(&["pin"]), &cancel)
            .is_err()
    );
    assert!(app.snapshots(fixture.host).unwrap().is_empty());
    drop(launch);
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
                accept_missing: false,
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
fn acknowledged_missing_dependencies_mark_the_session_and_result_unproven() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.root.join("minecraft/mods/base.jar")).unwrap();
    let app = fixture.app();
    let cancel = CancellationToken::new();

    app.begin_isolation(
        fixture.host,
        IsolationRequest {
            pinned: Vec::new(),
            accept_unrecorded: false,
            accept_missing: true,
        },
        &cancel,
    )
    .unwrap();

    let session = app.isolation_state(fixture.host).unwrap().unwrap();
    assert!(session.accepted_missing);
    assert!(session.reduced_confidence);
    assert!(!session.plan.missing.is_empty());
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

fn begin(fixture: &Fixture, pinned: &[&str]) -> (App, IsolationRound) {
    let app = fixture.app();
    let first = app
        .begin_isolation(fixture.host, request(pinned), &CancellationToken::new())
        .unwrap();
    (app, first)
}

fn feature_and_library() -> Fixture {
    Fixture::with_mods(&[
        ("feature", &["library"]),
        ("library", &[]),
        ("x", &[]),
        ("y", &[]),
    ])
}

#[test]
fn a_required_library_reproducing_alone_narrows_to_the_library_without_its_dependent() {
    let fixture = feature_and_library();
    let (app, first) = begin(&fixture, &[]);
    let (rounds, step) = drive(&app, fixture.host, first, enables(&["library"]));
    assert_dependency_closed(&fixture, &rounds);
    let IsolationStep::Complete(outcome) = step else {
        panic!("completed search");
    };
    assert_eq!(outcome.roots, mods(&["library"]));
    assert_eq!(outcome.confidence, IsolationConfidence::SmallestReached);
    let report = app.finish_isolation(fixture.host).unwrap();
    assert_eq!(roots(&report), mods(&["library"]));
    assert!(report.required_dependencies.is_empty());
    assert!(report.excluded_roots.contains(&mods(&["feature"])[0]));
    assert_eq!(enabled_files(&fixture), ["library.jar"]);
}

#[test]
fn a_feature_reproducing_with_its_library_keeps_the_library_as_a_required_dependency() {
    let fixture = feature_and_library();
    let (app, first) = begin(&fixture, &[]);
    let (rounds, step) = drive(&app, fixture.host, first, enables(&["feature"]));
    assert_dependency_closed(&fixture, &rounds);
    assert!(
        rounds
            .iter()
            .any(|round| round.enabled_roots == mods(&["library"])),
        "the library must be tested without the feature requiring it"
    );
    let IsolationStep::Complete(outcome) = step else {
        panic!("completed search");
    };
    assert_eq!(outcome.roots, mods(&["feature", "library"]));
    assert_eq!(outcome.confidence, IsolationConfidence::OneMinimal);
    let report = app.finish_isolation(fixture.host).unwrap();
    assert_eq!(roots(&report), mods(&["feature"]));
    assert_eq!(report.required_dependencies, mods(&["library"]));
    assert!(!report.excluded_roots.contains(&mods(&["library"])[0]));
}

#[test]
fn a_feature_mod_required_by_another_feature_mod_is_isolated_like_any_dependency() {
    let fixture = || {
        Fixture::with_mods(&[
            ("advancedae", &["ae2"]),
            ("ae2", &[]),
            ("x", &[]),
            ("y", &[]),
        ])
    };

    let dependency = fixture();
    let (app, first) = begin(&dependency, &[]);
    let (_, step) = drive(&app, dependency.host, first, enables(&["ae2"]));
    assert!(matches!(step, IsolationStep::Complete(outcome) if outcome.roots == mods(&["ae2"])));

    let dependent = fixture();
    let (app, first) = begin(&dependent, &[]);
    drive(&app, dependent.host, first, enables(&["advancedae"]));
    let report = app.finish_isolation(dependent.host).unwrap();
    assert_eq!(roots(&report), mods(&["advancedae"]));
    assert_eq!(report.required_dependencies, mods(&["ae2"]));
}

#[test]
fn a_required_cycle_is_searched_as_one_inseparable_deterministic_candidate() {
    let run = || {
        let fixture = Fixture::with_mods(&[("c", &["d"]), ("d", &["c"]), ("x", &[]), ("y", &[])]);
        let (app, first) = begin(&fixture, &[]);
        let (rounds, step) = drive(&app, fixture.host, first, enables(&["d"]));
        assert_dependency_closed(&fixture, &rounds);
        for round in &rounds {
            assert_eq!(
                round.enabled.contains(&mods(&["c"])[0]),
                round.enabled.contains(&mods(&["d"])[0])
            );
        }
        assert!(matches!(step, IsolationStep::Complete(_)));
        let report = app.finish_isolation(fixture.host).unwrap();
        assert_eq!(roots(&report), mods(&["c"]));
        assert_eq!(report.remaining_candidates[0].closure, mods(&["c", "d"]));
        assert_eq!(report.required_dependencies, mods(&["d"]));
        rounds
    };
    assert_eq!(run(), run());
}

#[test]
fn a_dependency_of_a_pinned_mod_is_reported_as_constrained_rather_than_ruled_out() {
    let fixture = Fixture::with_mods(&[
        ("pin", &["library"]),
        ("library", &[]),
        ("x", &["library"]),
        ("y", &[]),
    ]);
    let (app, first) = begin(&fixture, &["pin"]);
    let plan = app.isolation_state(fixture.host).unwrap().unwrap().plan;
    assert!(
        !plan
            .candidates
            .iter()
            .any(|candidate| candidate.root == mods(&["library"])[0])
    );
    let (rounds, _) = drive(&app, fixture.host, first, enables(&["library"]));
    assert!(
        rounds
            .iter()
            .all(|round| round.enabled.contains(&mods(&["library"])[0]))
    );
    let report = app.finish_isolation(fixture.host).unwrap();
    assert_eq!(report.pinned, mods(&["pin"]));
    assert_eq!(report.pinned_dependencies, mods(&["library"]));
    assert!(
        !report
            .required_dependencies
            .contains(&mods(&["library"])[0])
    );
    assert!(!report.excluded_roots.contains(&mods(&["library"])[0]));
}

#[test]
fn restart_resume_and_undo_preserve_the_dependency_aware_search() {
    let fixture = feature_and_library();
    let (app, first) = begin(&fixture, &[]);
    assert_eq!(first.enabled_roots, mods(&["feature", "library"]));
    let IsolationStep::AwaitingAnswer(library_alone) = app
        .answer_isolation_round(fixture.host, RoundAnswer::Reproduced)
        .unwrap()
    else {
        panic!("the library should be tested alone next");
    };
    assert_eq!(library_alone.enabled_roots, mods(&["library"]));

    let reopened = fixture.app();
    assert!(matches!(
        reopened.resume_isolation(fixture.host).unwrap(),
        IsolationStep::AwaitingAnswer(round) if round == library_alone
    ));
    assert_eq!(enabled_files(&fixture), ["library.jar"]);

    assert_eq!(reopened.undo_isolation_round(fixture.host).unwrap(), first);
    assert_eq!(enabled_files(&fixture), ["feature.jar", "library.jar"]);
    let IsolationStep::AwaitingAnswer(again) = reopened
        .answer_isolation_round(fixture.host, RoundAnswer::Reproduced)
        .unwrap()
    else {
        panic!("the same round should follow the same answer");
    };
    assert_eq!(again, library_alone);

    let (_, step) = drive(&reopened, fixture.host, again, enables(&["library"]));
    assert!(
        matches!(step, IsolationStep::Complete(outcome) if outcome.roots == mods(&["library"]))
    );
}

#[test]
fn any_single_culprit_is_reported_as_the_only_remaining_candidate() {
    for culprit in ["a", "b", "c", "d", "e", "shared"] {
        let fixture = Fixture::new();
        let (app, first) = begin(&fixture, &["pin"]);
        let (rounds, _) = drive(&app, fixture.host, first, enables(&[culprit]));
        assert_dependency_closed(&fixture, &rounds);
        let report = app.finish_isolation(fixture.host).unwrap();
        assert_eq!(roots(&report), mods(&[culprit]), "culprit {culprit}");
    }
}

#[test]
fn a_duplicate_made_during_a_session_carries_no_session_and_can_start_its_own() {
    let fixture = Fixture::new();
    let app = fixture.app();
    app.begin_isolation(fixture.host, request(&["pin"]), &CancellationToken::new())
        .unwrap();
    let InstalledHost::Instance(source) = fixture.host else {
        panic!("a client fixture");
    };

    let copy = app
        .duplicate_client(source, &DuplicateClient::default(), AbsentModPolicy::Keep)
        .unwrap()
        .record;

    assert!(
        fixture.root.join(".launcher/isolation.json").is_file(),
        "the original keeps its session"
    );
    assert!(!copy.root.join(".launcher/isolation.json").exists());
    let copied = InstalledHost::Instance(copy.id);
    assert!(app.isolation_state(copied).unwrap().is_none());
    assert!(app.isolation_state(fixture.host).unwrap().is_some());
}

fn assert_isolation_refusal<T: std::fmt::Debug>(result: Result<T, CoreError>) {
    match result {
        Err(CoreError::ChangeRefused {
            reason: RepairRefusal::IsolationActive,
        }) => {}
        other => panic!("expected an isolation refusal, got {other:?}"),
    }
}

#[test]
fn local_mod_changes_are_refused_while_a_session_owns_the_host() {
    let fixture = Fixture::new();
    let (app, _) = begin(&fixture, &["pin"]);
    let before = enabled_files(&fixture);
    let dropped = fixture.root.join("dropped.jar");
    std::fs::write(&dropped, b"dropped").unwrap();

    assert_isolation_refusal(app.set_mod_enabled(fixture.host, Path::new("mods/c.jar"), false));
    assert_isolation_refusal(
        app.set_mods_enabled(fixture.host, &[(mods(&["c"])[0].clone(), false)]),
    );
    assert_isolation_refusal(app.delete_mod_file(fixture.host, Path::new("mods/c.jar")));
    assert_isolation_refusal(app.delete_mod_files(fixture.host, &mods(&["c"])));
    assert_isolation_refusal(app.add_local_mods(fixture.host, &[dropped]));

    assert_eq!(enabled_files(&fixture), before);
    assert!(!fixture.root.join("minecraft/mods/dropped.jar").exists());
}

#[tokio::test]
async fn catalog_and_loader_changes_are_refused_while_a_session_owns_the_host() {
    let fixture = Fixture::new();
    let (app, _) = begin(&fixture, &["pin"]);
    let InstalledHost::Instance(instance) = fixture.host else {
        panic!("a client fixture");
    };
    let reporter = OperationReporter::silent("changing");
    let cancel = CancellationToken::new();
    let target = Path::new("mods/c.jar");

    assert_isolation_refusal(
        app.install_mods(
            instance,
            &ModInstallRequest::new("sodium"),
            &reporter,
            &cancel,
        )
        .await,
    );
    assert_isolation_refusal(
        app.change_managed_mod_version(
            instance,
            target,
            &ModVersionChoice::NewestCompatible,
            &reporter,
            &cancel,
        )
        .await,
    );
    assert_isolation_refusal(
        app.update_managed_mod(instance, target, &reporter, &cancel)
            .await,
    );
    assert_isolation_refusal(
        app.remove_managed_mod(instance, target, &reporter, &cancel)
            .await,
    );
    assert_isolation_refusal(
        app.restore_mod_file(fixture.host, target, &reporter, &cancel)
            .await,
    );
    assert_isolation_refusal(
        app.change_loader_version(instance, "0.0.0", true, &reporter, &cancel)
            .await,
    );
    assert_isolation_refusal(
        app.apply_linked_mod_operation(
            instance,
            &LinkedModRequest::Update {
                target: target.to_path_buf(),
            },
            &[],
            &reporter,
            &cancel,
        )
        .await,
    );
}

#[test]
fn mod_changes_work_again_once_the_session_ends() {
    let fixture = Fixture::new();
    let (app, _) = begin(&fixture, &["pin"]);
    app.cancel_isolation(fixture.host, &CancellationToken::new())
        .unwrap();
    app.set_mod_enabled(fixture.host, Path::new("mods/c.jar"), false)
        .unwrap();
    assert!(fixture.root.join("minecraft/mods/c.jar.disabled").is_file());

    let (app, first) = begin(&fixture, &["pin"]);
    drive(&app, fixture.host, first, enables(&["e"]));
    app.finish_isolation(fixture.host).unwrap();
    app.set_mod_enabled(fixture.host, Path::new("mods/c.jar"), true)
        .unwrap();
}

#[test]
fn rounds_resume_and_finish_still_apply_while_mod_changes_are_refused() {
    let fixture = Fixture::new();
    let (app, first) = begin(&fixture, &["pin"]);
    assert_isolation_refusal(app.set_mod_enabled(fixture.host, Path::new("mods/e.jar"), false));

    interrupt_round(&fixture);
    app.resume_isolation(fixture.host).unwrap();
    let (_, step) = drive(&app, fixture.host, first, enables(&["e"]));
    assert!(matches!(step, IsolationStep::Complete(_)));
    let report = app.finish_isolation(fixture.host).unwrap();
    assert_eq!(roots(&report), mods(&["e"]));
}
