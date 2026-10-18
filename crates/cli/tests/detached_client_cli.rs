//! A client game owned by a runner keeps running when its starter is gone, and
//! a later `ephervex` process finds it, lists it, and ends it.
//!
//! The runner is the real `ephervex` binary started with its internal argument,
//! handed a launch whose "game" is a shell script, exactly as a starting
//! process hands it a prepared command. Nothing here needs Minecraft, an
//! account, or a network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use ephervex_model::{APP_ID, LoaderSelection, StorageClass};
use ephervex_storage::{LaunchLock, LaunchState, NewClient, Paths, Registry, RunnerHost};
use tempfile::TempDir;

mod binary_sandbox;

struct Sandbox {
    directory: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temp dir");
        for child in ["config", "data", "cache", "state", "tmp"] {
            std::fs::create_dir_all(directory.path().join(child)).expect("create root");
        }
        Self { directory }
    }

    fn base(&self) -> &Path {
        self.directory.path()
    }

    fn sandboxed(&self, mut command: Command) -> Command {
        let base = self.base();
        command
            .env("HOME", base)
            .env("XDG_CONFIG_HOME", base.join("config"))
            .env("XDG_DATA_HOME", base.join("data"))
            .env("XDG_CACHE_HOME", base.join("cache"))
            .env("XDG_STATE_HOME", base.join("state"))
            .env("TMPDIR", base.join("tmp"))
            .env("EPHERVEX_LOG", "error");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = binary_sandbox::ephervex_command();
        command.args(args);
        self.sandboxed(command)
            .output()
            .expect("run the ephervex binary")
    }

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

    /// A client instance, returned as its id and root.
    fn client(&self) -> (String, PathBuf) {
        let registry = Registry::open(self.paths()).expect("open the registry");
        let client = registry
            .create_client(
                NewClient::new("Hosted", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Persistent),
            )
            .expect("create the client");
        registry
            .publish_client(&client)
            .expect("publish the client");
        (client.id.to_string(), client.root)
    }

    /// Starts the binary as a runner for a game that runs `script`, after
    /// taking the instance's lock the way a starting process does.
    fn start_runner(&self, root: &Path, script: &str) -> (Child, LaunchLock) {
        let starter = LaunchLock::acquire(root).expect("the starter's lock");
        let game_directory = root.join("minecraft");
        std::fs::create_dir_all(&game_directory).expect("game directory");
        let spec = serde_json::json!({
            "host": "client",
            "command": {
                "java": "/bin/sh",
                "working_directory": game_directory,
                "main_class": "",
                "arguments": [
                    {"value": "-c", "sensitive": false},
                    {"value": script, "sensitive": false},
                ],
                "jvm_argument_count": 2,
                "environment": [],
            },
            "instance_root": root,
            "additional": null,
            "previous": starter.state(),
            "run": uuid::Uuid::new_v4(),
            "capture_directory": root.join("capture"),
            "cache_directory": self.base().join("cache").join(APP_ID),
        });
        let mut command = binary_sandbox::ephervex_command();
        command
            .arg("--ephervex-runner")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut runner = self.sandboxed(command).spawn().expect("start the runner");
        let mut input = runner.stdin.take().expect("the runner's input");
        writeln!(input, "{spec}").expect("hand over the launch");
        // The starter keeps neither end open from here on: it is "gone".
        drop(input);
        (runner, starter)
    }
}

/// The game's process id, from the runner's first report.
fn running_pid(runner: &mut Child) -> u32 {
    let stdout = runner.stdout.take().expect("the runner's reports");
    let mut reports = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            reports.read_line(&mut line).expect("a report") > 0,
            "the runner ended before the game ran"
        );
        let message: serde_json::Value = serde_json::from_str(&line).expect("a message");
        match message["message"].as_str() {
            Some("running") => return u32::try_from(message["pid"].as_u64().unwrap()).unwrap(),
            Some("finished") => panic!("the game never ran: {message}"),
            _ => {}
        }
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(") ")
            .is_some_and(|(_, rest)| !rest.starts_with(['Z', 'X']))
    })
}

fn session_of(pid: &str) -> u32 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("a live process");
    stat.rsplit_once(") ")
        .expect("a stat line")
        .1
        .split_whitespace()
        .nth(3)
        .expect("a session field")
        .parse()
        .expect("a session id")
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Kills a process the test started so a failed assertion leaves nothing behind.
struct Reaper(Vec<u32>);

impl Drop for Reaper {
    fn drop(&mut self) {
        for pid in &self.0 {
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
    }
}

#[test]
fn a_game_keeps_running_after_its_starter_is_gone_and_a_later_command_ends_it() {
    let sandbox = Sandbox::new();
    let (id, root) = sandbox.client();
    let (mut runner, starter) = sandbox.start_runner(&root, "sleep 60");
    let runner_pid = runner.id();
    let mut cleanup = Reaper(vec![runner_pid]);

    let game = running_pid(&mut runner);
    cleanup.0.push(game);
    // The starter is gone: it dropped both pipes, and keeps only the handle on a
    // lock the runner has since taken over.
    drop(starter);

    assert!(alive(game) && alive(runner_pid));
    assert_ne!(session_of(&runner_pid.to_string()), session_of("self"));
    let shown = sandbox.run(&["running", "--json"]);
    assert!(shown.status.success(), "{}", stderr_of(&shown));
    let listing: serde_json::Value = serde_json::from_slice(&shown.stdout).expect("JSON");
    let row = &listing["running"][0];
    assert_eq!(row["id"], id.as_str());
    assert_eq!(row["pid"], game);
    assert_eq!(row["owner_pid"], runner_pid);
    assert_eq!(row["controllable"], true);

    let stopped = sandbox.run(&["instance", "stop", &id]);
    assert!(stopped.status.success(), "{}", stderr_of(&stopped));
    wait_until("the game to end", || !alive(game));
    wait_until("the runner to finish", || {
        runner.try_wait().expect("wait").is_some()
    });
    // Both are gone and reaped, so their ids may already belong to something else.
    cleanup.0.clear();
    let after = sandbox.run(&["running", "--json"]);
    let listing: serde_json::Value = serde_json::from_slice(&after.stdout).expect("JSON");
    assert!(
        listing["running"].as_array().is_some_and(Vec::is_empty),
        "the instance is free again: {}",
        stdout_of(&after)
    );
}

#[test]
fn a_game_that_ends_by_itself_is_reported_and_releases_the_instance() {
    let sandbox = Sandbox::new();
    let (_id, root) = sandbox.client();
    let (mut runner, _starter) = sandbox.start_runner(&root, "echo up; exit 3");
    let mut cleanup = Reaper(vec![runner.id()]);

    let stdout = runner.stdout.take().expect("reports");
    let reports: Vec<serde_json::Value> = BufReader::new(stdout)
        .lines()
        .map(|line| serde_json::from_str(&line.expect("a report")).expect("a message"))
        .collect();
    cleanup.0.clear();

    let last = reports.last().expect("a final report");
    assert_eq!(last["message"], "finished");
    assert_eq!(last["result"]["Ok"]["code"], 3);
    assert!(
        reports
            .iter()
            .any(|report| report["message"] == "output" && report["record"]["message"] == "up")
    );
    assert!(runner.wait().expect("runner").success());
    assert!(ephervex_storage::running_state(&root).is_none());
}

/// A lock whose runner record points at processes that are not a runner.
fn plant_lock(root: &Path, owner: u32, owner_ticks: u64, game: u32, game_ticks: u64) {
    let state = LaunchState {
        owner_pid: owner,
        owner_start_ticks: Some(owner_ticks),
        game_pid: Some(game),
        game_start_ticks: Some(game_ticks),
        since: chrono::Utc::now(),
        runner: Some(RunnerHost {
            run: uuid::Uuid::new_v4(),
            control_socket: root.join("no-such-socket"),
        }),
    };
    std::fs::write(
        root.join(".launcher").join("launch.lock"),
        serde_json::to_vec(&state).expect("json"),
    )
    .expect("plant the lock");
}

fn start_time(pid: u32) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("a live process");
    stat.rsplit_once(')')
        .expect("a stat line")
        .1
        .split_whitespace()
        .nth(19)
        .expect("start time")
        .parse()
        .expect("a number")
}

fn sleeper() -> Child {
    Command::new("sleep").arg("60").spawn().expect("a process")
}

#[test]
fn a_recorded_runner_that_is_not_the_process_it_names_is_never_adopted_or_signaled() {
    let sandbox = Sandbox::new();
    let (id, root) = sandbox.client();
    std::fs::create_dir_all(root.join(".launcher")).expect("metadata directory");
    let mut stranger = sleeper();
    let _cleanup = Reaper(vec![stranger.id()]);
    // The recorded start time belongs to some other process: the id was reused.
    plant_lock(
        &root,
        stranger.id(),
        start_time(stranger.id()).wrapping_add(1),
        stranger.id(),
        start_time(stranger.id()).wrapping_add(1),
    );

    let stopped = sandbox.run(&["instance", "stop", &id]);

    assert!(!stopped.status.success());
    assert!(alive(stranger.id()), "nothing was signaled");
    assert!(stranger.try_wait().expect("wait").is_none());
}

#[test]
fn an_unrelated_process_with_a_matching_id_and_start_time_but_no_run_marker_is_not_the_game() {
    let sandbox = Sandbox::new();
    let (id, root) = sandbox.client();
    std::fs::create_dir_all(root.join(".launcher")).expect("metadata directory");
    let mut stranger = sleeper();
    let _cleanup = Reaper(vec![stranger.id()]);
    let ticks = start_time(stranger.id());
    // Owner and game both name the stranger with its real start time, but the
    // stranger carries no run marker and serves no control socket.
    plant_lock(&root, stranger.id(), ticks, stranger.id(), ticks);

    let stopped = sandbox.run(&["instance", "stop", &id]);

    assert!(
        !stopped.status.success(),
        "no runner answers: {}",
        stdout_of(&stopped)
    );
    assert!(alive(stranger.id()), "the stranger was left alone");
    let state = ephervex_storage::running_state(&root).expect("the lock still holds the instance");
    assert!(
        !state.game_is_alive(),
        "a process without the marker is not the game"
    );
    assert!(stranger.try_wait().expect("wait").is_none());
}
