//! Inspecting logs, crash reports, and launcher diagnostics through the binary.
//!
//! The point of the feature is that nobody has to know where any of these files
//! are, so the test never builds a path to hand to the CLI: it writes the files
//! where the game would, then asks `ephervex logs` for them by name.
//!
//! Each run is pointed at a throwaway home through the XDG environment
//! variables, so the launcher's own log file lands in the sandbox rather than
//! in the developer's state directory.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

mod support;

/// A credential-shaped string, spelled so a substring search cannot miss it and
/// cannot match anything innocent.
const FIXTURE_SECRET: &str = "FAKE-MICROSOFT-ACCESS-TOKEN-INSIDE-A-GAME-LOG";

const CRASH_REPORT: &str = "---- Minecraft Crash Report ----\n\
     // Everything's going to plan. No, really, that was supposed to happen.\n\
     \n\
     Time: 2026-08-09 10:30:00\n\
     Description: Ticking entity\n\
     \n\
     java.lang.NullPointerException: Ticking entity\n\
     \tat net.minecraft.world.entity.Entity.tick(Entity.java:100)\n";

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
        self.run_with_log_level(args, "error")
    }

    /// Runs the binary with the launcher's own log filter set explicitly, so a
    /// test about what reaches the launcher log does not depend on the default.
    fn run_with_log_level(&self, args: &[&str], level: &str) -> Output {
        let base = self.base();
        Command::new(env!("CARGO_BIN_EXE_ephervex"))
            .args(args)
            .env("HOME", base)
            .env("XDG_CONFIG_HOME", base.join("config"))
            .env("XDG_DATA_HOME", base.join("data"))
            .env("XDG_CACHE_HOME", base.join("cache"))
            .env("XDG_STATE_HOME", base.join("state"))
            .env("TMPDIR", base.join("tmp"))
            .env("EPHERVEX_LOG", level)
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

    /// Everything the launcher has written about itself, concatenated.
    fn launcher_log(&self) -> String {
        let directory = self
            .base()
            .join("state")
            .join("ephervex")
            .join("logs")
            .join("launcher");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&directory)
            .unwrap_or_else(|error| {
                panic!(
                    "no launcher log directory at {}: {error}",
                    directory.display()
                )
            })
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        files.sort();
        files
            .iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .collect()
    }
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, body).expect("write");
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Creates an instance and returns its id and game directory.
fn instance_with_diagnostics(sandbox: &Sandbox) -> (String, PathBuf) {
    let record = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Crash repro",
        "--minecraft",
        "1.20.1",
    ]);
    let id = record["id"].as_str().expect("an id").to_owned();
    let game = PathBuf::from(record["root"].as_str().expect("a root")).join("minecraft");

    write(
        &game.join("logs").join("latest.log"),
        "[main/INFO]: Setting user: Notch\n[main/ERROR]: Exception in tick loop\n",
    );
    write(&game.join("logs").join("debug.log"), "[debug] verbose\n");
    write(
        &game
            .join("crash-reports")
            .join("crash-2026-08-09_10.30.00-client.txt"),
        CRASH_REPORT,
    );
    (id, game)
}

#[test]
fn a_clients_logs_and_crash_reports_are_inspected_without_naming_a_path() {
    let sandbox = Sandbox::new();
    let (id, game) = instance_with_diagnostics(&sandbox);

    // --- list ---------------------------------------------------------------
    let logs = sandbox.json(&["--json", "logs", "list", "--instance", &id]);
    let names: Vec<&str> = logs
        .as_array()
        .expect("an array")
        .iter()
        .map(|entry| entry["name"].as_str().expect("a name"))
        .collect();
    assert!(names.contains(&"latest.log"), "{names:?}");
    assert!(names.contains(&"debug.log"), "{names:?}");

    // --- open ---------------------------------------------------------------
    let shown = sandbox.run(&["logs", "show", "--instance", &id, "latest.log"]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    assert!(stdout(&shown).contains("Exception in tick loop"));

    // --- search -------------------------------------------------------------
    let found = sandbox.run(&[
        "logs",
        "search",
        "--instance",
        &id,
        "latest.log",
        "exception",
    ]);
    assert!(found.status.success(), "{}", stderr(&found));
    assert_eq!(
        stdout(&found).trim(),
        "2:[main/ERROR]: Exception in tick loop",
        "the match carries the line it was found on"
    );
    let missing = sandbox.run(&[
        "logs",
        "search",
        "--instance",
        &id,
        "latest.log",
        "nothing-here",
    ]);
    assert!(missing.status.success(), "no matches is not a failure");
    assert!(stdout(&missing).trim().is_empty());

    // Searching does not modify what it searched.
    assert!(
        std::fs::read_to_string(game.join("logs").join("latest.log"))
            .expect("read")
            .contains("Exception in tick loop")
    );

    // --- copy the path ------------------------------------------------------
    let path = sandbox.run(&["logs", "show", "--instance", &id, "latest.log", "--path"]);
    assert_eq!(
        Path::new(stdout(&path).trim()),
        game.join("logs").join("latest.log")
    );

    // --- the containing folder ----------------------------------------------
    let folder = sandbox.run(&["logs", "open", "--instance", &id, "--print"]);
    assert_eq!(Path::new(stdout(&folder).trim()), game.join("logs"));
    let crash_folder = sandbox.run(&[
        "logs",
        "open",
        "--instance",
        &id,
        "--crash-reports",
        "--print",
    ]);
    assert_eq!(
        Path::new(stdout(&crash_folder).trim()),
        game.join("crash-reports")
    );
}

#[test]
fn the_newest_crash_report_is_listed_first_and_summarized() {
    let sandbox = Sandbox::new();
    let (id, game) = instance_with_diagnostics(&sandbox);

    // An older report, and one whose format nothing recognizes.
    write(
        &game
            .join("crash-reports")
            .join("crash-2026-08-01-client.txt"),
        CRASH_REPORT,
    );
    write(
        &game.join("crash-reports").join("crash-unfamiliar.txt"),
        "a mod wrote this in its own shape\n",
    );
    for (name, seconds_ago) in [
        ("crash-2026-08-09_10.30.00-client.txt", 0),
        ("crash-unfamiliar.txt", 60),
        ("crash-2026-08-01-client.txt", 120),
    ] {
        let file = std::fs::File::options()
            .write(true)
            .open(game.join("crash-reports").join(name))
            .expect("open");
        file.set_modified(
            std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(1_800_000_000 - seconds_ago),
        )
        .expect("set modification time");
    }

    let reports = sandbox.json(&[
        "--json",
        "logs",
        "list",
        "--instance",
        &id,
        "--crash-reports",
    ]);
    let reports = reports.as_array().expect("an array");
    assert_eq!(
        reports
            .iter()
            .map(|entry| entry["name"].as_str().expect("a name"))
            .collect::<Vec<_>>(),
        [
            "crash-2026-08-09_10.30.00-client.txt",
            "crash-unfamiliar.txt",
            "crash-2026-08-01-client.txt",
        ]
    );
    assert_eq!(
        reports[0]["summary"]["description"], "Ticking entity",
        "a familiar report is summarized"
    );
    assert_eq!(
        reports[1]["summary"]["description"],
        serde_json::Value::Null,
        "an unfamiliar one recognizes nothing..."
    );
    assert_eq!(
        reports[1]["summary"]["excerpt"][0], "a mod wrote this in its own shape",
        "...and is still described by its own first lines"
    );

    // ...and stays openable, which is the property that matters.
    let shown = sandbox.run(&[
        "logs",
        "show",
        "--instance",
        &id,
        "--crash-reports",
        "crash-unfamiliar.txt",
    ]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    assert!(stdout(&shown).contains("a mod wrote this in its own shape"));
}

#[test]
fn an_instance_that_has_never_run_reports_nothing_rather_than_failing() {
    let sandbox = Sandbox::new();
    let record = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Untouched",
        "--minecraft",
        "1.20.1",
    ]);
    let id = record["id"].as_str().expect("an id");

    let logs = sandbox.json(&["--json", "logs", "list", "--instance", id]);
    assert_eq!(logs.as_array().expect("an array").len(), 0);

    // The folder action is unavailable rather than creating an empty folder
    // that would suggest the game had run.
    let folder = sandbox.run(&["logs", "open", "--instance", id, "--print"]);
    assert!(!folder.status.success());
    assert!(
        stderr(&folder).contains("does not exist yet"),
        "{}",
        stderr(&folder)
    );

    let missing = sandbox.run(&["logs", "show", "--instance", id, "latest.log"]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("no longer there"),
        "{}",
        stderr(&missing)
    );
}

#[test]
fn a_file_outside_the_instance_cannot_be_read_through_the_log_browser() {
    let sandbox = Sandbox::new();
    let (id, _) = instance_with_diagnostics(&sandbox);
    let outside = sandbox.base().join("credentials.log");
    write(&outside, &format!("{FIXTURE_SECRET}\n"));

    for name in ["../../../../credentials.log", "..", "/etc/passwd"] {
        let refused = sandbox.run(&["logs", "show", "--instance", &id, name]);
        assert!(!refused.status.success(), "{name} must be refused");
        assert!(
            !stdout(&refused).contains(FIXTURE_SECRET),
            "{name} leaked a file outside the instance"
        );
    }
}

#[test]
fn a_log_that_is_not_text_is_still_inspectable() {
    let sandbox = Sandbox::new();
    let (id, game) = instance_with_diagnostics(&sandbox);
    std::fs::write(
        game.join("logs").join("latest.log"),
        b"[main] before \xff\xfe after\n",
    )
    .expect("write");

    let shown = sandbox.run(&["logs", "show", "--instance", &id, "latest.log"]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    assert!(stdout(&shown).contains("before"));
    assert!(stdout(&shown).contains("after"));
    assert!(
        stderr(&shown).contains("not text"),
        "the user is told the file had bytes that are not text: {}",
        stderr(&shown)
    );

    // An empty log reads as empty rather than failing.
    std::fs::write(game.join("logs").join("latest.log"), b"").expect("write");
    let empty = sandbox.run(&["logs", "show", "--instance", &id, "latest.log"]);
    assert!(empty.status.success(), "{}", stderr(&empty));
    assert!(stdout(&empty).is_empty());
}

#[test]
fn a_huge_log_is_shown_in_a_bounded_window_that_says_how_much_it_covers() {
    let sandbox = Sandbox::new();
    let (id, game) = instance_with_diagnostics(&sandbox);
    let line = "[main/INFO]: a mod says something at length, repeatedly\n";
    std::fs::write(game.join("logs").join("debug.log"), line.repeat(40_000)).expect("write");

    let shown = sandbox.run(&[
        "logs",
        "show",
        "--instance",
        &id,
        "debug.log",
        "--bytes",
        "4096",
    ]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    assert!(
        stdout(&shown).len() <= 4096,
        "the window is bounded: {} bytes",
        stdout(&shown).len()
    );
    assert!(stderr(&shown).contains("Showing"), "{}", stderr(&shown));
}

#[test]
fn a_search_in_json_says_whether_it_covered_the_whole_file_and_decoded_it_cleanly() {
    let sandbox = Sandbox::new();
    let (id, game) = instance_with_diagnostics(&sandbox);
    let line = "[main/INFO]: a mod says something at length, repeatedly\n";
    let log = game.join("logs").join("debug.log");
    std::fs::write(&log, format!("needle\n{}", line.repeat(40_000))).expect("write");
    let file_bytes = std::fs::metadata(&log).expect("metadata").len();

    let windowed = sandbox.json(&[
        "--json",
        "logs",
        "search",
        "--instance",
        &id,
        "debug.log",
        "needle",
        "--bytes",
        "4096",
    ]);
    assert_eq!(windowed["file_bytes"], file_bytes, "{windowed}");
    assert!(windowed["returned_bytes"].as_u64().expect("a size") <= 4096);
    assert_eq!(windowed["window_truncated"], true, "{windowed}");
    assert_eq!(windowed["lossy"], false, "{windowed}");
    assert_eq!(
        windowed["truncated"], false,
        "the match-limit flag does not mean the window was cut: {windowed}"
    );
    assert_eq!(windowed["matches"].as_array().expect("matches").len(), 0);

    let whole = sandbox.json(&[
        "--json",
        "logs",
        "search",
        "--instance",
        &id,
        "debug.log",
        "needle",
        "--bytes",
        &file_bytes.to_string(),
    ]);
    assert_eq!(whole["window_truncated"], false, "{whole}");
    assert_eq!(whole["returned_bytes"], file_bytes);
    assert_eq!(whole["matches"].as_array().expect("matches").len(), 1);

    std::fs::write(&log, b"needle \xff\xfe\n").expect("write");
    let lossy = sandbox.json(&[
        "--json",
        "logs",
        "search",
        "--instance",
        &id,
        "debug.log",
        "needle",
    ]);
    assert_eq!(lossy["lossy"], true, "{lossy}");
    assert_eq!(lossy["window_truncated"], false, "{lossy}");
}

#[test]
fn the_launcher_keeps_a_log_of_its_own_that_can_be_listed_and_opened() {
    let sandbox = Sandbox::new();
    // Any command at all writes to it.
    sandbox.run_with_log_level(&["info"], "info");

    let logs = sandbox.json(&["--json", "logs", "list", "--launcher"]);
    let entries = logs.as_array().expect("an array");
    assert!(
        !entries.is_empty(),
        "the launcher writes its own log: {logs}"
    );
    let name = entries[0]["name"].as_str().expect("a name").to_owned();

    let shown = sandbox.run(&["logs", "show", "--launcher", &name]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    assert!(
        !sandbox.launcher_log().contains('\u{1b}'),
        "the launcher log is plain text: it is read in a viewer and pasted into reports"
    );

    let folder = sandbox.run(&["logs", "open", "--launcher", "--print"]);
    assert!(folder.status.success(), "{}", stderr(&folder));
    assert_eq!(
        Path::new(stdout(&folder).trim()),
        sandbox
            .base()
            .join("state")
            .join("ephervex")
            .join("logs")
            .join("launcher")
    );
}

#[test]
fn a_launch_reports_an_operation_id_that_is_findable_in_the_launcher_log() {
    let sandbox = Sandbox::new();
    let record = sandbox.json(&[
        "--json",
        "instance",
        "create",
        "Correlate me",
        "--minecraft",
        "1.20.1",
    ]);
    let id = record["id"].as_str().expect("an id");

    // Nobody is signed in, so this fails early - which is exactly the case
    // where an operation id has to have been reported already.
    let attempt = sandbox.run_with_log_level(&["launch", id], "info");
    assert!(!attempt.status.success());

    let reported = stderr(&attempt)
        .lines()
        .find_map(|line| line.strip_prefix("Operation ").map(str::to_owned))
        .expect("the launch reports its operation id");

    let launcher_log = sandbox.launcher_log();
    assert!(
        launcher_log.contains(&reported),
        "the reported operation id must appear in the launcher log:\n{launcher_log}"
    );
    assert!(
        launcher_log.contains("launch failed"),
        "the failure is recorded under that id:\n{launcher_log}"
    );

    // ...and the same id is findable through the log browser itself.
    let entries = sandbox.json(&["--json", "logs", "list", "--launcher"]);
    let name = entries.as_array().expect("an array")[0]["name"]
        .as_str()
        .expect("a name")
        .to_owned();
    let found = sandbox.run(&["logs", "search", "--launcher", &name, &reported]);
    assert!(found.status.success(), "{}", stderr(&found));
    assert!(
        stdout(&found).contains(&reported),
        "searching the launcher log finds the operation: {}",
        stdout(&found)
    );
}

#[test]
fn reading_a_log_never_copies_its_contents_into_the_launcher_log() {
    // The launcher log is a newly exposed diagnostic surface. A game log can
    // contain anything, including a credential a mod printed, and browsing one
    // must not duplicate it into a second file the user did not choose.
    let sandbox = Sandbox::new();
    let (id, game) = instance_with_diagnostics(&sandbox);
    write(
        &game.join("logs").join("latest.log"),
        &format!("[main/INFO]: token={FIXTURE_SECRET}\n"),
    );

    let shown =
        sandbox.run_with_log_level(&["logs", "show", "--instance", &id, "latest.log"], "debug");
    assert!(shown.status.success(), "{}", stderr(&shown));
    assert!(
        stdout(&shown).contains(FIXTURE_SECRET),
        "the user asked for this file's contents"
    );

    sandbox.run_with_log_level(&["logs", "list", "--instance", &id], "debug");
    sandbox.run_with_log_level(
        &["logs", "search", "--instance", &id, "latest.log", "token"],
        "debug",
    );

    let launcher_log = sandbox.launcher_log();
    assert!(
        !launcher_log.contains(FIXTURE_SECRET),
        "a log's contents must not reach the launcher log:\n{launcher_log}"
    );
    assert!(
        !stderr(&shown).contains(FIXTURE_SECRET),
        "the commentary stream does not repeat the file either"
    );
}

#[test]
fn the_log_commands_document_themselves_without_naming_internals() {
    let sandbox = Sandbox::new();
    let help = sandbox.run(&["logs", "--help"]);
    assert!(help.status.success());
    let text = stdout(&help);
    for expected in ["list", "show", "search", "open"] {
        assert!(
            text.contains(expected),
            "{expected} is missing from:\n{text}"
        );
    }
}
