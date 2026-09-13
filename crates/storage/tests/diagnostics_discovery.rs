//! Discovery, ordering, containment, and bounded reading of diagnostic files.
//!
//! Everything here runs against real directories in a temporary root: the
//! properties under test - what a listing skips, what a symlink does, what a
//! window contains when the file changed underneath it - are properties of the
//! filesystem, and a fake one would prove nothing about them.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ephervex_storage::diagnostics::{
    DEFAULT_VIEW_BYTES, DiagnosticError, DiagnosticKind, MAX_VIEW_BYTES, OwnedDirectory, Refusal,
    TextAnchor, read_view,
};

const CRASH_REPORT: &str = "---- Minecraft Crash Report ----\n\
     // Who set us up the TNT?\n\
     \n\
     Time: 2026-08-09 10:30:00\n\
     Description: Ticking entity\n\
     \n\
     java.lang.NullPointerException: Ticking entity\n";

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, body).expect("write");
}

/// Sets a file's modification time so ordering can be asserted rather than
/// raced against the filesystem's clock resolution.
fn set_modified(path: &Path, seconds_ago: u64) {
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open");
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 - seconds_ago))
        .expect("set modification time");
}

/// The layout a client instance actually has.
fn client_logs(root: &Path) -> OwnedDirectory {
    OwnedDirectory::new(
        root,
        root.join("minecraft").join("logs"),
        DiagnosticKind::Log,
    )
}

fn client_crashes(root: &Path) -> OwnedDirectory {
    OwnedDirectory::new(
        root,
        root.join("minecraft").join("crash-reports"),
        DiagnosticKind::CrashReport,
    )
}

fn server_logs(root: &Path) -> OwnedDirectory {
    OwnedDirectory::new(root, root.join("server").join("logs"), DiagnosticKind::Log)
}

fn names(directory: &OwnedDirectory) -> Vec<String> {
    directory
        .entries()
        .expect("entries")
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

#[test]
fn a_clients_latest_and_debug_logs_are_discovered_and_compressed_rotations_are_not() {
    let base = tempfile::tempdir().expect("temp dir");
    let root = base.path().join("instance");
    let logs = root.join("minecraft").join("logs");
    write(&logs.join("latest.log"), "[main] hello\n");
    write(&logs.join("debug.log"), "[debug] hello\n");
    // A rotated log is gzipped. It is not text, and decompressing untrusted
    // archive data to browse a log is a surface this feature does not open.
    write(&logs.join("2026-08-09-1.log.gz"), "not really gzip");

    let found = names(&client_logs(&root));
    assert!(found.contains(&"latest.log".to_owned()), "{found:?}");
    assert!(found.contains(&"debug.log".to_owned()), "{found:?}");
    assert!(
        !found
            .iter()
            .any(|name| Path::new(name).extension().is_some_and(|ext| ext == "gz")),
        "{found:?}"
    );
}

#[test]
fn a_servers_own_logs_and_crash_reports_are_discovered_in_the_server_directory() {
    let base = tempfile::tempdir().expect("temp dir");
    let root = base.path().join("server-instance");
    write(
        &root.join("server").join("logs").join("latest.log"),
        "[Server thread/INFO]: Done\n",
    );
    write(
        &root
            .join("server")
            .join("crash-reports")
            .join("crash-2026-08-09_10.30.00-server.txt"),
        CRASH_REPORT,
    );

    assert_eq!(names(&server_logs(&root)), ["latest.log"]);

    let crashes = OwnedDirectory::new(
        &root,
        root.join("server").join("crash-reports"),
        DiagnosticKind::CrashReport,
    );
    let entries = crashes.entries().expect("entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]
            .summary
            .as_ref()
            .and_then(|summary| summary.description.clone())
            .as_deref(),
        Some("Ticking entity")
    );
}

#[test]
fn a_missing_log_directory_lists_nothing_rather_than_failing() {
    // An instance that has never been launched is a normal state to render.
    let base = tempfile::tempdir().expect("temp dir");
    let root = base.path().join("instance");
    std::fs::create_dir_all(&root).expect("mkdir");

    assert!(client_logs(&root).entries().expect("entries").is_empty());
    assert!(client_crashes(&root).entries().expect("entries").is_empty());

    let error = client_logs(&root)
        .resolve("latest.log")
        .expect_err("nothing to resolve");
    assert!(matches!(error, DiagnosticError::Missing { .. }));
}

#[test]
fn crash_reports_are_listed_newest_first_with_the_name_breaking_ties() {
    let base = tempfile::tempdir().expect("temp dir");
    let root = base.path().join("instance");
    let crashes = root.join("minecraft").join("crash-reports");

    write(&crashes.join("crash-2026-08-07-client.txt"), CRASH_REPORT);
    write(&crashes.join("crash-2026-08-09-client.txt"), CRASH_REPORT);
    // Two reports written in the same second: whole-second filesystem
    // timestamps make this the ordinary case, not an exotic one.
    write(&crashes.join("crash-2026-08-08a-client.txt"), CRASH_REPORT);
    write(&crashes.join("crash-2026-08-08b-client.txt"), CRASH_REPORT);

    set_modified(&crashes.join("crash-2026-08-09-client.txt"), 0);
    set_modified(&crashes.join("crash-2026-08-08a-client.txt"), 60);
    set_modified(&crashes.join("crash-2026-08-08b-client.txt"), 60);
    set_modified(&crashes.join("crash-2026-08-07-client.txt"), 120);

    let expected = [
        "crash-2026-08-09-client.txt",
        "crash-2026-08-08b-client.txt",
        "crash-2026-08-08a-client.txt",
        "crash-2026-08-07-client.txt",
    ];
    assert_eq!(names(&client_crashes(&root)), expected);
    // Deterministic: a second listing of the same directory agrees with the
    // first, whatever order the filesystem happened to return entries in.
    assert_eq!(names(&client_crashes(&root)), expected);
}

#[test]
fn a_symlinked_entry_is_neither_listed_nor_opened() {
    let base = tempfile::tempdir().expect("temp dir");
    let root = base.path().join("instance");
    let logs = root.join("minecraft").join("logs");
    write(&logs.join("latest.log"), "[main] hello\n");

    let secret = base.path().join("credentials.log");
    write(&secret, "FAKE-REFRESH-TOKEN\n");
    std::os::unix::fs::symlink(&secret, logs.join("linked.log")).expect("symlink");

    assert_eq!(
        names(&client_logs(&root)),
        ["latest.log"],
        "a link is not a log this instance wrote"
    );

    let error = client_logs(&root)
        .resolve("linked.log")
        .expect_err("a link must not be followed");
    assert!(matches!(
        error,
        DiagnosticError::Refused {
            reason: Refusal::Symlink,
            ..
        }
    ));
}

#[test]
fn a_log_directory_that_leaves_the_instance_is_refused_entirely() {
    // The directory itself is the thing being replaced here, so refusing the
    // entry is not enough: the listing must fail too.
    let base = tempfile::tempdir().expect("temp dir");
    let root = base.path().join("instance");
    std::fs::create_dir_all(root.join("minecraft")).expect("mkdir");

    let elsewhere = base.path().join("elsewhere");
    write(&elsewhere.join("latest.log"), "FAKE-REFRESH-TOKEN\n");
    std::os::unix::fs::symlink(&elsewhere, root.join("minecraft").join("logs")).expect("symlink");

    let error = client_logs(&root).entries().expect_err("must be refused");
    assert!(matches!(
        error,
        DiagnosticError::Refused {
            reason: Refusal::OutsideOwnedRoot,
            ..
        }
    ));
    assert_eq!(error.message_id(), "error-diagnostics-file-refused");

    assert!(client_logs(&root).resolve("latest.log").is_err());
}

#[test]
fn a_link_that_stays_inside_the_instance_is_allowed() {
    // Containment is the rule, not "no links anywhere": an instance whose game
    // directory is reached through a link inside its own root still works.
    let base = tempfile::tempdir().expect("temp dir");
    let root = base.path().join("instance");
    let real = root.join("game-data").join("logs");
    write(&real.join("latest.log"), "[main] hello\n");
    std::fs::create_dir_all(root.join("minecraft")).expect("mkdir");
    std::os::unix::fs::symlink(&real, root.join("minecraft").join("logs")).expect("symlink");

    assert_eq!(names(&client_logs(&root)), ["latest.log"]);
}

#[test]
fn a_large_log_is_read_in_a_bounded_window() {
    let base = tempfile::tempdir().expect("temp dir");
    let root = base.path().join("instance");
    let path = root.join("minecraft").join("logs").join("debug.log");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");

    // Larger than the default window and larger than the hard ceiling would
    // allow a caller to ask for in one go.
    let line = "[main] a mod is very talkative indeed and says so at length\n";
    let body = line.repeat(20_000);
    assert!(u64::try_from(body.len()).expect("size") > DEFAULT_VIEW_BYTES);
    std::fs::write(&path, &body).expect("write");

    let view = read_view(&path, TextAnchor::Tail, DEFAULT_VIEW_BYTES).expect("read");
    assert!(view.truncated);
    assert!(view.returned_bytes <= DEFAULT_VIEW_BYTES);
    assert!(
        view.text.ends_with(line),
        "the end of the file is what a log window shows"
    );

    // A caller asking for everything still gets a bounded window.
    let greedy = read_view(&path, TextAnchor::Tail, u64::MAX).expect("read");
    assert!(greedy.returned_bytes <= MAX_VIEW_BYTES);
}

#[test]
fn a_log_truncated_or_replaced_between_reads_still_reads() {
    let base = tempfile::tempdir().expect("temp dir");
    let root = base.path().join("instance");
    let logs = root.join("minecraft").join("logs");
    let path = logs.join("latest.log");
    write(&path, "[main] first session\n[main] still going\n");

    let first = read_view(&path, TextAnchor::Tail, DEFAULT_VIEW_BYTES).expect("read");
    assert!(first.text.contains("still going"));

    // Minecraft truncates `latest.log` when it starts again.
    std::fs::write(&path, "").expect("truncate");
    let emptied = read_view(&path, TextAnchor::Tail, DEFAULT_VIEW_BYTES).expect("refresh");
    assert_eq!(emptied.text, "");
    assert_eq!(emptied.file_bytes, 0);

    // ...and then writes the new session into it.
    std::fs::write(&path, "[main] second session\n").expect("write");
    let refreshed = read_view(&path, TextAnchor::Tail, DEFAULT_VIEW_BYTES).expect("refresh");
    assert_eq!(refreshed.text, "[main] second session\n");

    // A rotation that replaces the file with a different inode reads the new
    // file, because the path is resolved again on every read.
    std::fs::remove_file(&path).expect("remove");
    std::fs::write(&path, "[main] third session\n").expect("write");
    assert_eq!(
        read_view(&path, TextAnchor::Tail, DEFAULT_VIEW_BYTES)
            .expect("refresh")
            .text,
        "[main] third session\n"
    );

    // A file that rotated away between the listing and the read says so.
    std::fs::remove_file(&path).expect("remove");
    let error = client_logs(&root)
        .resolve("latest.log")
        .expect_err("it is gone");
    assert!(matches!(error, DiagnosticError::Missing { .. }));
}

#[test]
fn an_entry_name_is_resolved_only_inside_the_directory_that_owns_it() {
    let base = tempfile::tempdir().expect("temp dir");
    let root = base.path().join("instance");
    write(
        &root.join("minecraft").join("logs").join("latest.log"),
        "[main] hello\n",
    );
    let outside = base.path().join("credentials.log");
    write(&outside, "FAKE-REFRESH-TOKEN\n");

    let logs = client_logs(&root);
    let resolved = logs.resolve("latest.log").expect("a real entry resolves");
    assert!(resolved.ends_with(PathBuf::from("logs").join("latest.log")));

    for name in [
        "../../../credentials.log",
        "..",
        ".",
        "",
        "logs/latest.log",
        "/etc/passwd",
    ] {
        let error = logs.resolve(name).expect_err("must be refused");
        assert!(
            matches!(
                error,
                DiagnosticError::Refused {
                    reason: Refusal::NotOneComponent,
                    ..
                } | DiagnosticError::Missing { .. }
            ),
            "{name}: {error}"
        );
    }
}

#[test]
fn a_directory_entry_is_never_offered_as_a_file() {
    let base = tempfile::tempdir().expect("temp dir");
    let root = base.path().join("instance");
    let logs = root.join("minecraft").join("logs");
    std::fs::create_dir_all(logs.join("archive.log")).expect("mkdir");

    assert!(names(&client_logs(&root)).is_empty());
    let error = client_logs(&root)
        .resolve("archive.log")
        .expect_err("a directory is not a log");
    assert!(matches!(
        error,
        DiagnosticError::Refused {
            reason: Refusal::NotARegularFile,
            ..
        }
    ));
}
