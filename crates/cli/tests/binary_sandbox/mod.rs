//! The one way an integration test starts the `ephervex` binary.
//!
//! A spawned binary is a release-profile build of the real program, so any
//! code path that opens the credential store would reach the developer's
//! Secret Service through the session bus and raise a keyring unlock prompt.
//! Test doubles cannot cover that. Starting every binary through
//! [`ephervex_command`] cuts the process off from the bus instead: the
//! Secret Service is found through `DBUS_SESSION_BUS_ADDRESS`. The dead address
//! set here is what blocks it. Removing `XDG_RUNTIME_DIR` only adds depth: a
//! client with no address falls back to `/run/user/<uid>/bus`, normally the
//! real bus, so a test must never clear the environment or reset either
//! variable (ADR 0004).
//!
//! `bus_isolation_cli.rs` fails when a test starts the binary any other way or
//! touches the variables that carry the isolation.

use std::process::Command;

/// A session bus address with nothing behind it.
pub(crate) const UNREACHABLE_SESSION_BUS: &str = "unix:path=/nonexistent/ephervex-test-session-bus";

/// A command for the binary under test that cannot reach the session bus.
///
/// The caller still points `HOME` and the XDG variables at a sandbox.
pub(crate) fn ephervex_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ephervex"));
    command
        .env("DBUS_SESSION_BUS_ADDRESS", UNREACHABLE_SESSION_BUS)
        .env_remove("XDG_RUNTIME_DIR");
    command
}
