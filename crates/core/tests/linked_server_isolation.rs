//! An isolation session on a client keeps the linked servers it names in step with each round.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ephervex_core::{
    App, CoreError, FileDisposition, InstalledHost, IsolationError, IsolationRequest,
    IsolationRound, IsolationStep, Paths, RepairRefusal, RoundAnswer, SnapshotError,
    SnapshotRestoreOptions,
};
use ephervex_model::{
    InstanceId, LoaderSelection, ServerId, ServerInstallation, ServerLaunch, ServerRelativePath,
};
use ephervex_storage::{NewClient, NewServer, Registry};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// A client and a linked server declaring the same projects.
///
/// The server names its jar for project `c` differently, has one mod only it
/// has, and records a client-only mod it does not load.
struct Pair {
    _directory: tempfile::TempDir,
    paths: Paths,
    client: InstanceId,
    client_root: PathBuf,
    server: ServerId,
    server_root: PathBuf,
}

fn client_file(name: &str, client: &str, server: &str, relations: Option<Value>) -> Value {
    let mut file = json!({
        "logical_path": format!("mods/{name}.jar"), "hashes": [],
        "downloads": [], "size": name.len(), "client": client, "server": server,
        "origin": "user_selected",
        "identity": {"provider": "modrinth", "project_id": name, "version_id": format!("{name}1")},
    });
    if let Some(relations) = relations {
        file["relations"] = relations;
    }
    file
}

fn recipe(files: &[Value]) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "recipe_version":1, "source_kind":"manual", "original_name":"Pair",
        "minecraft_version":"1.20.1","loader":{"kind":"vanilla","version":null},
        "files":files, "override_layers":[], "provider_metadata":{"kind":"manual"},
    }))
    .unwrap()
}

fn write(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

impl Pair {
    fn new() -> Self {
        Self::with_server_relations(Some(&json!([])))
    }

    /// `server_relations` is what the server's recipe records for every mod it
    /// loads, `None` meaning nothing has asked a catalog yet.
    fn with_server_relations(server_relations: Option<&Value>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(directory.path());
        let registry = Registry::open(paths.clone()).unwrap();
        let client = registry
            .create_client(NewClient::new("Pair", "1.20.1", LoaderSelection::vanilla()))
            .unwrap();
        registry.publish_client(&client).unwrap();
        let mut server = registry
            .create_server(NewServer::linked("Linked", client.id))
            .unwrap();
        server.installation = ServerInstallation::Installed {
            launch: ServerLaunch::Jar {
                path: ServerRelativePath::new("server.jar").unwrap(),
            },
        };
        registry.update_server(&mut server).unwrap();

        let mut client_files = Vec::new();
        for name in ["a", "b", "c", "d", "clientonly"] {
            write(
                &client.root,
                &format!("minecraft/mods/{name}.jar"),
                name.as_bytes(),
            );
            let server_side = if name == "clientonly" {
                "unsupported"
            } else {
                "required"
            };
            client_files.push(client_file(name, "required", server_side, Some(json!([]))));
        }
        write(
            &client.root,
            ".launcher/source/recipe.json",
            &recipe(&client_files),
        );

        let mut server_files = Vec::new();
        for name in ["a", "b", "c", "d", "serveronly"] {
            let jar = if name == "c" { "c-server" } else { name };
            write(
                &server.root,
                &format!("server/mods/{jar}.jar"),
                jar.as_bytes(),
            );
            let client_side = if name == "serveronly" {
                "unsupported"
            } else {
                "required"
            };
            let mut file = client_file(name, client_side, "required", server_relations.cloned());
            file["logical_path"] = json!(format!("mods/{jar}.jar"));
            server_files.push(file);
        }
        server_files.push(client_file("clientonly", "required", "unsupported", None));
        write(
            &server.root,
            ".launcher/source/recipe.json",
            &recipe(&server_files),
        );
        Self {
            _directory: directory,
            paths,
            client: client.id,
            client_root: client.root,
            server: server.id,
            server_root: server.root,
        }
    }

    fn app(&self) -> App {
        App::open_at(self.paths.clone()).unwrap()
    }

    const fn client_host(&self) -> InstalledHost {
        InstalledHost::Instance(self.client)
    }

    const fn server_host(&self) -> InstalledHost {
        InstalledHost::Server(self.server)
    }

    fn request(&self) -> IsolationRequest {
        IsolationRequest {
            pinned: Vec::new(),
            accept_unrecorded: false,
            accept_missing: false,
            linked_servers: vec![self.server],
        }
    }

    fn begin(&self, app: &App) -> IsolationRound {
        app.begin_isolation(
            self.client_host(),
            self.request(),
            &CancellationToken::new(),
        )
        .unwrap()
    }

    fn server_jars(&self) -> BTreeSet<String> {
        jars(&self.server_root.join("server/mods"))
    }

    fn client_jars(&self) -> BTreeSet<String> {
        jars(&self.client_root.join("minecraft/mods"))
    }

    /// Marks the server as running by leaving the evidence a supervisor would.
    fn mark_server_running(&self) {
        let evidence = ephervex_server::RuntimeEvidence::for_current_supervisor().unwrap();
        let path = ephervex_server::runtime_path(&self.server_root);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&evidence).unwrap()).unwrap();
    }

    fn mark_server_stopped(&self) {
        std::fs::remove_file(ephervex_server::runtime_path(&self.server_root)).unwrap();
    }
}

fn jars(directory: &Path) -> BTreeSet<String> {
    std::fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
        })
        .collect()
}

/// The jars the server must load when the client loads `client`.
fn expected_server_jars(client: &BTreeSet<String>) -> BTreeSet<String> {
    let mut expected = BTreeSet::from(["serveronly.jar".to_owned()]);
    for name in ["a", "b", "c", "d"] {
        if client.contains(&format!("{name}.jar")) {
            expected.insert(if name == "c" {
                "c-server.jar".to_owned()
            } else {
                format!("{name}.jar")
            });
        }
    }
    expected
}

fn assert_in_step(pair: &Pair) {
    assert_eq!(
        pair.server_jars(),
        expected_server_jars(&pair.client_jars()),
        "the server's mods do not match the client's"
    );
}

#[test]
fn a_followed_server_enables_what_the_client_round_enables() {
    let pair = Pair::new();
    let app = pair.app();
    let mut round = pair.begin(&app);
    assert_in_step(&pair);
    let mut tested = 0;
    loop {
        let reproduces = round.enabled.contains(&PathBuf::from("mods/c.jar"));
        let answer = if reproduces {
            RoundAnswer::Reproduced
        } else {
            RoundAnswer::DidNotReproduce
        };
        tested += 1;
        match app
            .answer_isolation_round(pair.client_host(), answer)
            .unwrap()
        {
            IsolationStep::AwaitingAnswer(next) => {
                assert_in_step(&pair);
                round = next;
            }
            IsolationStep::Complete(_) => break,
            IsolationStep::Contradiction(_) => panic!("unexpected contradiction"),
        }
    }
    assert!(tested > 1, "the search narrowed in one round");
    assert_in_step(&pair);

    let report = app.finish_isolation(pair.client_host()).unwrap();

    assert_eq!(
        pair.server_jars(),
        BTreeSet::from(["c-server.jar".to_owned(), "serveronly.jar".to_owned()])
    );
    assert!(report.linked_servers.is_empty());
    let [server] = report.servers.as_slice() else {
        panic!("one followed server");
    };
    assert_eq!(server.server, pair.server);
    assert_eq!(server.untested, [PathBuf::from("mods/serveronly.jar")]);
    assert!(server.enabled.contains(&PathBuf::from("mods/c-server.jar")));
    assert!(server.disabled.contains(&PathBuf::from("mods/a.jar")));
    // The server's own snapshot is retained under a dated label.
    let snapshots = app.snapshots(pair.server_host()).unwrap();
    assert!(
        snapshots
            .iter()
            .any(|snapshot| snapshot.id == server.safety_snapshot
                && snapshot.label.starts_with("Before mod isolation, ")),
        "{snapshots:?}"
    );
}

#[test]
fn undoing_a_round_puts_the_followed_server_back_with_the_client() {
    let pair = Pair::new();
    let app = pair.app();
    let first = pair.begin(&app);
    let first_client = pair.client_jars();
    app.answer_isolation_round(pair.client_host(), RoundAnswer::DidNotReproduce)
        .unwrap();
    assert_ne!(pair.client_jars(), first_client);

    let undone = app.undo_isolation_round(pair.client_host()).unwrap();

    assert_eq!(undone.enabled, first.enabled);
    assert_eq!(pair.client_jars(), first_client);
    assert_in_step(&pair);
}

#[test]
fn a_server_the_request_does_not_name_is_listed_and_left_alone() {
    let pair = Pair::new();
    let app = pair.app();
    let before = pair.server_jars();
    let mut request = pair.request();
    request.linked_servers.clear();

    app.begin_isolation(pair.client_host(), request, &CancellationToken::new())
        .unwrap();

    assert_eq!(pair.server_jars(), before);
    let session = app.isolation_state(pair.client_host()).unwrap().unwrap();
    assert!(session.followed_servers.is_empty());
    assert_eq!(session.plan.linked_servers.len(), 1);
    assert!(app.isolation_leader(pair.server).unwrap().is_none());
    assert!(app.snapshots(pair.server_host()).unwrap().is_empty());
}

#[test]
fn a_followed_server_refuses_its_own_changes_while_the_client_session_runs() {
    let pair = Pair::new();
    let app = pair.app();
    pair.begin(&app);
    let session = app.isolation_state(pair.client_host()).unwrap().unwrap();
    let snapshot = session.followed_servers[0].safety_snapshot;

    let leader = app.isolation_leader(pair.server).unwrap().unwrap();
    assert_eq!(leader.client, pair.client);
    assert_eq!(leader.safety_snapshot, snapshot);

    assert!(matches!(
        app.set_mod_enabled(pair.server_host(), Path::new("mods/a.jar"), true),
        Err(CoreError::ChangeRefused {
            reason: RepairRefusal::IsolationActive
        })
    ));
    assert!(matches!(
        app.restore_snapshot(
            pair.server_host(),
            snapshot,
            SnapshotRestoreOptions::default(),
            &CancellationToken::new()
        ),
        Err(SnapshotError::IsolationActive)
    ));
    assert!(matches!(
        app.delete_snapshot(pair.server_host(), snapshot),
        Err(CoreError::Isolation(
            IsolationError::SafetySnapshotInUse { .. }
        ))
    ));
    assert!(matches!(
        app.begin_isolation(
            pair.server_host(),
            IsolationRequest {
                pinned: Vec::new(),
                accept_unrecorded: true,
                accept_missing: true,
                linked_servers: Vec::new(),
            },
            &CancellationToken::new()
        ),
        Err(CoreError::Isolation(IsolationError::OwnedElsewhere { .. }))
    ));
    assert_in_step(&pair);
}

#[test]
fn a_followed_server_is_released_when_the_session_finishes_or_is_canceled() {
    let pair = Pair::new();
    let app = pair.app();
    pair.begin(&app);
    app.cancel_isolation(pair.client_host(), &CancellationToken::new())
        .unwrap();
    assert!(app.isolation_leader(pair.server).unwrap().is_none());
    app.set_mod_enabled(pair.server_host(), Path::new("mods/a.jar"), false)
        .expect("the server is free once the session ended");
}

#[test]
fn canceling_restores_each_followed_server_from_its_own_snapshot() {
    let pair = Pair::new();
    let app = pair.app();
    let server_before = pair.server_jars();
    let client_before = pair.client_jars();
    pair.begin(&app);
    app.answer_isolation_round(pair.client_host(), RoundAnswer::DidNotReproduce)
        .unwrap();
    assert_ne!(pair.server_jars(), server_before);

    app.cancel_isolation(pair.client_host(), &CancellationToken::new())
        .unwrap();

    assert_eq!(pair.server_jars(), server_before);
    assert_eq!(pair.client_jars(), client_before);
    assert!(app.isolation_state(pair.client_host()).unwrap().is_none());
    assert!(app.isolation_leader(pair.server).unwrap().is_none());
}

#[test]
fn starting_with_a_server_not_linked_to_the_client_changes_nothing() {
    let pair = Pair::new();
    let app = pair.app();
    let stranger = Registry::open(pair.paths.clone())
        .unwrap()
        .create_server(NewServer::new(
            "Stranger",
            "1.20.1",
            LoaderSelection::vanilla(),
        ))
        .unwrap();
    let mut request = pair.request();
    request.linked_servers = vec![stranger.id];
    let before = pair.client_jars();

    let refused = app.begin_isolation(pair.client_host(), request, &CancellationToken::new());

    assert!(matches!(
        refused,
        Err(CoreError::Isolation(IsolationError::ServerNotLinked { .. }))
    ));
    assert_eq!(pair.client_jars(), before);
    assert!(app.isolation_state(pair.client_host()).unwrap().is_none());
    assert!(app.snapshots(pair.client_host()).unwrap().is_empty());
}

#[test]
fn a_server_with_its_own_session_cannot_join_a_client_session() {
    let pair = Pair::new();
    let app = pair.app();
    app.begin_isolation(
        pair.server_host(),
        IsolationRequest {
            pinned: Vec::new(),
            accept_unrecorded: true,
            accept_missing: true,
            linked_servers: Vec::new(),
        },
        &CancellationToken::new(),
    )
    .unwrap();

    let refused = app.begin_isolation(
        pair.client_host(),
        pair.request(),
        &CancellationToken::new(),
    );

    assert!(matches!(
        refused,
        Err(CoreError::Isolation(IsolationError::OwnedElsewhere { .. }))
    ));
    assert!(app.isolation_state(pair.client_host()).unwrap().is_none());
    assert!(app.snapshots(pair.client_host()).unwrap().is_empty());
}

#[test]
fn a_running_followed_server_blocks_starting_and_every_round_change() {
    let pair = Pair::new();
    let app = pair.app();
    pair.mark_server_running();
    assert!(matches!(
        app.begin_isolation(
            pair.client_host(),
            pair.request(),
            &CancellationToken::new()
        ),
        Err(CoreError::Isolation(IsolationError::HostRunning { .. }))
    ));
    assert!(app.snapshots(pair.client_host()).unwrap().is_empty());

    pair.mark_server_stopped();
    pair.begin(&app);
    pair.mark_server_running();
    let drifted = pair.client_jars();
    assert!(
        app.answer_isolation_round(pair.client_host(), RoundAnswer::Reproduced)
            .is_err()
    );
    assert!(app.undo_isolation_round(pair.client_host()).is_err());
    assert!(app.finish_isolation(pair.client_host()).is_err());
    assert!(
        app.cancel_isolation(pair.client_host(), &CancellationToken::new())
            .is_err()
    );
    assert_eq!(pair.client_jars(), drifted);
    assert!(app.isolation_state(pair.client_host()).unwrap().is_some());

    pair.mark_server_stopped();
    app.answer_isolation_round(pair.client_host(), RoundAnswer::Reproduced)
        .unwrap();
    assert_in_step(&pair);
}

#[test]
fn reconciling_reapplies_a_followed_servers_round_after_an_interruption() {
    let pair = Pair::new();
    let app = pair.app();
    pair.begin(&app);
    let intended = pair.server_jars();
    let enabled = intended
        .iter()
        .find(|name| name.as_str() != "serveronly.jar")
        .unwrap()
        .clone();
    let jar = pair.server_root.join("server/mods").join(&enabled);
    std::fs::rename(&jar, jar.with_extension("jar.disabled")).unwrap();

    assert!(matches!(
        app.answer_isolation_round(pair.client_host(), RoundAnswer::Reproduced),
        Err(CoreError::Isolation(IsolationError::NotApplied { .. }))
    ));
    assert_eq!(
        app.reconcile_isolation(pair.client_host()).unwrap(),
        ephervex_core::IsolationReconciliation::Reapplied
    );
    assert_eq!(pair.server_jars(), intended);
}

#[test]
fn a_marker_left_without_its_session_holds_nothing() {
    let pair = Pair::new();
    let app = pair.app();
    pair.begin(&app);
    let marker = pair.server_root.join(".launcher/isolation-leader.json");
    let kept = std::fs::read(&marker).unwrap();
    app.finish_isolation(pair.client_host())
        .expect_err("the first round is still open");
    app.cancel_isolation(pair.client_host(), &CancellationToken::new())
        .unwrap();
    std::fs::write(&marker, kept).unwrap();

    assert!(app.isolation_leader(pair.server).unwrap().is_none());
    app.set_mod_enabled(pair.server_host(), Path::new("mods/a.jar"), false)
        .expect("a stale marker does not lock the server");
}

#[test]
fn a_followed_servers_unrecorded_dependencies_need_the_same_acknowledgement() {
    let pair = Pair::with_server_relations(None);
    let app = pair.app();

    let refused = app.begin_isolation(
        pair.client_host(),
        pair.request(),
        &CancellationToken::new(),
    );
    assert!(matches!(
        refused,
        Err(CoreError::Isolation(
            IsolationError::UnrecordedDependencies { .. }
        ))
    ));
    assert!(app.snapshots(pair.server_host()).unwrap().is_empty());

    let mut request = pair.request();
    request.accept_unrecorded = true;
    app.begin_isolation(pair.client_host(), request, &CancellationToken::new())
        .unwrap();
    let session = app.isolation_state(pair.client_host()).unwrap().unwrap();
    assert!(session.accepted_unrecorded);
    assert!(session.reduced_confidence);
}

#[test]
fn the_plan_lists_a_linked_servers_dependency_gaps_before_it_is_chosen() {
    let pair = Pair::with_server_relations(None);
    let app = pair.app();

    let plan = app.plan_isolation(pair.client_host(), &[]).unwrap();

    let [server] = plan.linked_servers.as_slice() else {
        panic!("one linked server");
    };
    assert_eq!(server.id, pair.server.to_string());
    assert!(server.installed);
    assert!(!server.unrecorded.is_empty());
}

#[test]
fn a_session_document_without_followed_servers_loads_and_follows_none() {
    let pair = Pair::new();
    let app = pair.app();
    let mut request = pair.request();
    request.linked_servers.clear();
    app.begin_isolation(pair.client_host(), request, &CancellationToken::new())
        .unwrap();
    let path = pair.client_root.join(".launcher/isolation.json");
    let mut document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    document.as_object_mut().unwrap().remove("followed_servers");
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();

    let session = app.isolation_state(pair.client_host()).unwrap().unwrap();

    assert!(session.followed_servers.is_empty());
    assert!(session.server_configurations().is_empty());
    app.cancel_isolation(pair.client_host(), &CancellationToken::new())
        .unwrap();
}

/// Answers every round by whether it enables `reproducing`, running `check`
/// after each step, and returns once the search completes.
fn drive_checking(pair: &Pair, app: &App, reproducing: &str, check: impl Fn()) {
    let mut round = pair.begin(app);
    loop {
        check();
        let answer = if round.enabled.contains(&PathBuf::from(reproducing)) {
            RoundAnswer::Reproduced
        } else {
            RoundAnswer::DidNotReproduce
        };
        match app
            .answer_isolation_round(pair.client_host(), answer)
            .unwrap()
        {
            IsolationStep::AwaitingAnswer(next) => round = next,
            IsolationStep::Complete(_) => break,
            IsolationStep::Contradiction(_) => panic!("unexpected contradiction"),
        }
    }
    check();
}

#[test]
fn deleting_a_followed_server_is_refused_until_the_session_ends() {
    let pair = Pair::new();
    let app = pair.app();
    pair.begin(&app);

    assert!(matches!(
        app.delete_server(pair.server, FileDisposition::Keep),
        Err(CoreError::Isolation(IsolationError::OwnedByClient { .. }))
    ));
    app.cancel_isolation(pair.client_host(), &CancellationToken::new())
        .expect("the session can still end");

    app.delete_server(pair.server, FileDisposition::Keep)
        .expect("the server is free once the session ended");
}

#[test]
fn a_server_mod_whose_client_mod_is_disabled_stays_as_it_was_and_is_reported_untested() {
    let pair = Pair::new();
    let app = pair.app();
    let client_jar = pair.client_root.join("minecraft/mods/d.jar");
    std::fs::rename(&client_jar, client_jar.with_extension("jar.disabled")).unwrap();

    drive_checking(&pair, &app, "mods/c.jar", || {
        assert!(
            pair.server_jars().contains("d.jar"),
            "the server's d.jar was turned off for a mod no round tests"
        );
    });
    let report = app.finish_isolation(pair.client_host()).unwrap();

    let [server] = report.servers.as_slice() else {
        panic!("one followed server");
    };
    assert!(server.untested.contains(&PathBuf::from("mods/d.jar")));
    assert!(server.enabled.contains(&PathBuf::from("mods/d.jar")));
    assert!(pair.server_jars().contains("d.jar"));
}

#[test]
fn a_server_mod_the_server_had_disabled_stays_off_and_adds_no_dependency_gap() {
    let pair = Pair::new();
    let app = pair.app();
    let server_jar = pair.server_root.join("server/mods/b.jar");
    std::fs::rename(&server_jar, server_jar.with_extension("jar.disabled")).unwrap();
    let recipe = pair.server_root.join(".launcher/source/recipe.json");
    let mut document: Value = serde_json::from_slice(&std::fs::read(&recipe).unwrap()).unwrap();
    for file in document["files"].as_array_mut().unwrap() {
        if file["logical_path"] == "mods/b.jar" {
            file["relations"] =
                json!([{"kind":"required","target":{"project":{"project_id":"absent-library"}}}]);
        }
    }
    std::fs::write(&recipe, serde_json::to_vec(&document).unwrap()).unwrap();

    drive_checking(&pair, &app, "mods/c.jar", || {
        assert!(
            !pair.server_jars().contains("b.jar"),
            "a mod the server had off was turned on"
        );
    });
    let session = app.isolation_state(pair.client_host()).unwrap().unwrap();
    assert!(!session.accepted_missing);
}

#[test]
fn a_followed_server_stays_owned_when_its_marker_is_missing() {
    let pair = Pair::new();
    let app = pair.app();
    pair.begin(&app);
    std::fs::remove_file(pair.server_root.join(".launcher/isolation-leader.json")).unwrap();

    assert!(app.isolation_leader(pair.server).unwrap().is_some());
    assert!(matches!(
        app.set_mod_enabled(pair.server_host(), Path::new("mods/a.jar"), false),
        Err(CoreError::ChangeRefused {
            reason: RepairRefusal::IsolationActive
        })
    ));
}

#[test]
fn a_session_ends_even_when_a_followed_server_was_deleted_behind_it() {
    let pair = Pair::new();
    let app = pair.app();
    pair.begin(&app);
    Registry::open(pair.paths.clone())
        .unwrap()
        .delete_server(pair.server, FileDisposition::Remove)
        .unwrap();

    app.answer_isolation_round(pair.client_host(), RoundAnswer::DidNotReproduce)
        .expect("a deleted server does not block rounds");
    app.cancel_isolation(pair.client_host(), &CancellationToken::new())
        .expect("a deleted server does not block cancel");

    assert!(app.isolation_state(pair.client_host()).unwrap().is_none());
}

#[test]
fn a_finished_report_leaves_out_a_server_deleted_behind_the_session() {
    let pair = Pair::new();
    let app = pair.app();
    let mut round = pair.begin(&app);
    Registry::open(pair.paths.clone())
        .unwrap()
        .delete_server(pair.server, FileDisposition::Remove)
        .unwrap();
    loop {
        let answer = if round.enabled.contains(&PathBuf::from("mods/c.jar")) {
            RoundAnswer::Reproduced
        } else {
            RoundAnswer::DidNotReproduce
        };
        match app
            .answer_isolation_round(pair.client_host(), answer)
            .unwrap()
        {
            IsolationStep::AwaitingAnswer(next) => round = next,
            _ => break,
        }
    }

    let report = app.finish_isolation(pair.client_host()).unwrap();

    assert!(report.servers.is_empty());
}

#[test]
fn what_an_untouched_server_mod_requires_is_reported_untested_too() {
    let pair = Pair::new();
    let app = pair.app();
    let recipe = pair.server_root.join(".launcher/source/recipe.json");
    let mut document: Value = serde_json::from_slice(&std::fs::read(&recipe).unwrap()).unwrap();
    for file in document["files"].as_array_mut().unwrap() {
        if file["logical_path"] == "mods/serveronly.jar" {
            file["relations"] =
                json!([{"kind":"required","target":{"project":{"project_id":"a"}}}]);
        }
    }
    std::fs::write(&recipe, serde_json::to_vec(&document).unwrap()).unwrap();

    drive_checking(&pair, &app, "mods/c.jar", || {
        assert!(
            pair.server_jars().contains("a.jar"),
            "a mod the untouched serveronly.jar requires was turned off"
        );
    });
    let report = app.finish_isolation(pair.client_host()).unwrap();

    let [server] = report.servers.as_slice() else {
        panic!("one followed server");
    };
    assert!(server.untested.contains(&PathBuf::from("mods/a.jar")));
}

#[test]
fn a_cancel_that_fails_after_restoring_a_server_never_restores_it_again() {
    let pair = Pair::new();
    let app = pair.app();
    let server_before = pair.server_jars();
    pair.begin(&app);
    let session = app.isolation_state(pair.client_host()).unwrap().unwrap();
    let manifest = pair
        .client_root
        .join(".launcher/snapshots")
        .join(session.safety_snapshot.to_string())
        .join("snapshot.json");
    let intact = std::fs::read(&manifest).unwrap();
    std::fs::write(&manifest, b"not a manifest").unwrap();

    let failed = app.cancel_isolation(pair.client_host(), &CancellationToken::new());

    assert!(failed.is_err(), "the client restore should fail");
    assert_eq!(pair.server_jars(), server_before);
    let session = app.isolation_state(pair.client_host()).unwrap().unwrap();
    assert!(
        session.server_configurations().is_empty(),
        "a restored server is no longer followed"
    );
    assert_eq!(session.unchanged_linked_servers().len(), 1);
    // The server runs on after its restore; a retry must not undo that.
    std::fs::write(pair.server_root.join("server/mods/ran.jar"), b"ran").unwrap();
    std::fs::write(&manifest, intact).unwrap();

    app.cancel_isolation(pair.client_host(), &CancellationToken::new())
        .unwrap();

    assert!(pair.server_jars().contains("ran.jar"));
}

#[test]
fn a_session_ends_when_a_followed_servers_files_are_gone() {
    let pair = Pair::new();
    let app = pair.app();
    pair.begin(&app);
    std::fs::remove_dir_all(&pair.server_root).unwrap();

    app.answer_isolation_round(pair.client_host(), RoundAnswer::DidNotReproduce)
        .expect("a server without files does not block rounds");
    app.cancel_isolation(pair.client_host(), &CancellationToken::new())
        .expect("a server without files does not block cancel");

    assert!(app.isolation_state(pair.client_host()).unwrap().is_none());
}

#[test]
fn a_linked_servers_mod_changes_do_not_depend_on_its_clients_recipe() {
    let pair = Pair::new();
    let app = pair.app();
    std::fs::write(
        pair.client_root.join(".launcher/source/recipe.json"),
        b"not a recipe",
    )
    .unwrap();

    app.set_mod_enabled(pair.server_host(), Path::new("mods/a.jar"), false)
        .expect("the server's mods change without reading the client's recipe");
    app.delete_server(pair.server, FileDisposition::Keep)
        .expect("the server is removed without reading the client's recipe");
}

#[test]
fn a_followed_server_whose_folder_is_briefly_away_is_brought_back_in_step() {
    let pair = Pair::new();
    let app = pair.app();
    pair.begin(&app);
    let away = pair.server_root.with_extension("away");
    std::fs::rename(&pair.server_root, &away).unwrap();

    app.answer_isolation_round(pair.client_host(), RoundAnswer::DidNotReproduce)
        .expect("a server out of reach does not block rounds");
    std::fs::rename(&away, &pair.server_root).unwrap();

    assert_eq!(
        app.reconcile_isolation(pair.client_host()).unwrap(),
        ephervex_core::IsolationReconciliation::Reapplied
    );
    assert_in_step(&pair);
    assert!(app.isolation_leader(pair.server).unwrap().is_some());
}

#[test]
fn canceling_with_a_followed_server_out_of_reach_names_it_as_not_restored() {
    let pair = Pair::new();
    let app = pair.app();
    let server_before = pair.server_jars();
    pair.begin(&app);
    app.answer_isolation_round(pair.client_host(), RoundAnswer::DidNotReproduce)
        .unwrap();
    let away = pair.server_root.with_extension("away");
    std::fs::rename(&pair.server_root, &away).unwrap();

    let canceled = app
        .cancel_isolation(pair.client_host(), &CancellationToken::new())
        .unwrap();
    std::fs::rename(&away, &pair.server_root).unwrap();

    let [server] = canceled.unreached.as_slice() else {
        panic!("the unreached server is named");
    };
    assert_eq!(server.server, pair.server);
    assert_ne!(
        pair.server_jars(),
        server_before,
        "the server was not restored, as reported"
    );
}

#[test]
fn finishing_with_a_followed_server_out_of_reach_names_it_instead_of_reporting_it_set() {
    let pair = Pair::new();
    let app = pair.app();
    let mut round = pair.begin(&app);
    let away = pair.server_root.with_extension("away");
    std::fs::rename(&pair.server_root, &away).unwrap();
    loop {
        let answer = if round.enabled.contains(&PathBuf::from("mods/c.jar")) {
            RoundAnswer::Reproduced
        } else {
            RoundAnswer::DidNotReproduce
        };
        match app
            .answer_isolation_round(pair.client_host(), answer)
            .unwrap()
        {
            IsolationStep::AwaitingAnswer(next) => round = next,
            _ => break,
        }
    }

    let report = app.finish_isolation(pair.client_host()).unwrap();
    std::fs::rename(&away, &pair.server_root).unwrap();

    assert!(report.servers.is_empty());
    let [server] = report.unreached.as_slice() else {
        panic!("the unreached server is named");
    };
    assert_eq!(server.server, pair.server);
    assert!(report.linked_servers.is_empty());
}
