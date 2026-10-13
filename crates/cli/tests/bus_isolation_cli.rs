//! Every test that starts the `ephervex` binary goes through the shared
//! launcher that keeps it away from the developer's session bus, so none of
//! them can reach the Secret Service and raise a keyring unlock prompt.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::ffi::OsStr;
use std::path::Path;

mod binary_sandbox;

/// Text that starts the binary without the shared launcher, or undoes its
/// isolation. Clearing the environment drops the dead bus address and lets a
/// client fall back to `/run/user/<uid>/bus`.
const BYPASSES: &[&str] = &[
    "CARGO_BIN_EXE_ephervex",
    "env_clear",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_RUNTIME_DIR",
];

#[test]
fn the_shared_launcher_points_the_session_bus_nowhere_and_drops_the_runtime_directory() {
    let command = binary_sandbox::ephervex_command();
    let setting = |name: &str| {
        command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new(name))
            .map(|(_, value)| value)
    };

    assert_eq!(
        setting("DBUS_SESSION_BUS_ADDRESS"),
        Some(Some(OsStr::new(binary_sandbox::UNREACHABLE_SESSION_BUS)))
    );
    assert_eq!(
        setting("XDG_RUNTIME_DIR"),
        Some(None),
        "the runtime directory is removed as well, though the dead bus address is what blocks the bus"
    );
    let path = binary_sandbox::UNREACHABLE_SESSION_BUS
        .strip_prefix("unix:path=")
        .expect("a unix socket address");
    assert!(
        !Path::new(path).exists(),
        "the unreachable bus address must not exist"
    );
}

#[test]
fn no_integration_test_starts_the_binary_without_the_shared_launcher() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders = Vec::new();
    for entry in walk(&tests) {
        if entry.extension() != Some(OsStr::new("rs"))
            || entry.ends_with("binary_sandbox/mod.rs")
            || entry.ends_with("bus_isolation_cli.rs")
        {
            continue;
        }
        let source = std::fs::read_to_string(&entry).expect("read a test source");
        if BYPASSES.iter().any(|marker| source.contains(marker)) {
            offenders.push(entry.strip_prefix(&tests).unwrap().display().to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "these tests start the binary directly or change its bus isolation instead of using `binary_sandbox::ephervex_command()` as is: {offenders:?}"
    );
}

fn walk(directory: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory).expect("read a test directory") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}
