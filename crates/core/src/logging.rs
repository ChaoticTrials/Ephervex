//! Tracing setup shared by both frontends.
//!
//! Logs carry operation ids, instance ids, and process exit codes. They must
//! never carry refresh tokens, access tokens, the CurseForge API key, or
//! authorization headers — redaction is the responsibility of the crate
//! producing the event, since only it knows which fields are secret.

use std::sync::OnceLock;

use tracing_subscriber::{EnvFilter, fmt, prelude::*};

static INITIALIZED: OnceLock<()> = OnceLock::new();

/// Environment variable that overrides the log filter, e.g.
/// `EPHERVEX_LOG=ephervex_download=debug`.
pub const LOG_ENV_VAR: &str = "EPHERVEX_LOG";

/// Initializes the global tracing subscriber.
///
/// Safe to call more than once: repeated calls are ignored rather than
/// panicking, so a CLI entry point and an embedded test harness can both call
/// it without coordinating.
pub fn init_tracing(default_directives: &str) {
    INITIALIZED.get_or_init(|| {
        let filter = EnvFilter::try_from_env(LOG_ENV_VAR)
            .unwrap_or_else(|_| EnvFilter::new(default_directives));

        // Writing to stderr keeps stdout clean for `--json` CLI output.
        let layer = fmt::layer().with_writer(std::io::stderr).with_target(true);

        // A failed install means another subscriber already owns the global
        // slot, which is not worth aborting a launch over.
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(layer)
            .try_init();
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn repeated_initialization_is_harmless() {
        init_tracing("info");
        init_tracing("debug");
    }
}
