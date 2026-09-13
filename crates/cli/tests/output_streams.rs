//! What the command line does when whatever was reading it stops.
//!
//! `head`, `less` and `grep -q` all close the pipe before a long document is
//! finished. Rust ignores `SIGPIPE` at startup, so the next write returns
//! `EPIPE` to the program rather than killing it, and anything that unwraps
//! that error turns an ordinary shell pipeline into a panic: a stack trace, a
//! non-zero status, and a build path printed at the user.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Read as _;
use std::process::{Command, Stdio};

/// Runs a command, reads a little of its output, then stops reading.
///
/// Returns the exit status and whatever reached standard error, which is where
/// a panic would show up.
fn abandoned_midway(args: &[&str]) -> (Option<i32>, String) {
    let base = tempfile::tempdir().expect("temp dir");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ephervex"))
        .args(args)
        .env("HOME", base.path())
        .env("XDG_CONFIG_HOME", base.path().join("config"))
        .env("XDG_DATA_HOME", base.path().join("data"))
        .env("XDG_CACHE_HOME", base.path().join("cache"))
        .env("XDG_STATE_HOME", base.path().join("state"))
        .env("TMPDIR", base.path().join("tmp"))
        .env("EPHERVEX_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the ephervex binary");

    let mut stdout = child.stdout.take().expect("stdout");
    let mut sip = [0_u8; 64];
    let _ = stdout.read(&mut sip);
    // What `head` does: stop reading and close the pipe.
    drop(stdout);

    let mut stderr = String::new();
    if let Some(mut handle) = child.stderr.take() {
        let _ = handle.read_to_string(&mut stderr);
    }
    let status = child.wait().expect("wait");
    (status.code(), stderr)
}

/// The reproducible case: the completion script is far larger than a pipe
/// buffer, so the write that fails is guaranteed rather than a race.
#[test]
fn a_completion_script_piped_into_something_that_stops_reading_does_not_panic() {
    let (code, stderr) = abandoned_midway(&["completions", "bash"]);

    assert!(
        !stderr.contains("panicked"),
        "generating completions into a closed pipe panicked:\n{stderr}"
    );
    assert_ne!(
        code,
        Some(101),
        "exit 101 is a panic; a closed pipe is an ordinary way to end a pipeline"
    );
}

/// The same exposure through the other bulk writer: a machine-readable listing
/// is exactly what gets piped into something that reads one line.
#[test]
fn a_json_listing_piped_into_something_that_stops_reading_does_not_panic() {
    let (code, stderr) = abandoned_midway(&["--json", "instances", "list"]);

    assert!(
        !stderr.contains("panicked"),
        "a JSON listing into a closed pipe panicked:\n{stderr}"
    );
    assert_ne!(code, Some(101), "exit 101 is a panic");
}

/// The fix must not cost the output itself: a completion script is only useful
/// whole, and a reader that does not go away has to receive all of it.
#[test]
fn a_completion_script_read_to_the_end_is_still_complete() {
    let base = tempfile::tempdir().expect("temp dir");
    let output = Command::new(env!("CARGO_BIN_EXE_ephervex"))
        .args(["completions", "bash"])
        .env("HOME", base.path())
        .env("EPHERVEX_LOG", "error")
        .output()
        .expect("run the ephervex binary");

    assert!(output.status.success());
    let script = String::from_utf8_lossy(&output.stdout);
    assert!(
        script.contains("ephervex"),
        "the completion script does not mention the command it completes"
    );
    assert!(
        script.lines().count() > 100,
        "a truncated completion script is worse than none: {} lines",
        script.lines().count()
    );
}
