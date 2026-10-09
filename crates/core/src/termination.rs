//! Requests to end the process: Ctrl-C and, on Unix, a closing terminal or a
//! termination request.
//!
//! Both frontends turn these into their normal way of quitting, so whatever
//! the process supervises is stopped instead of left running.

/// Ctrl-C and, on Unix, the terminal closing or a termination request.
///
/// The signals stay registered for as long as this value lives, so a request
/// that arrives between two waits is not lost. [`TerminationRequests::watch`]
/// must run inside a Tokio runtime with its I/O driver enabled.
#[derive(Debug)]
pub struct TerminationRequests {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    /// Absent when the process started with hangups ignored, as under
    /// `nohup`: registering would replace that choice with a stop.
    #[cfg(unix)]
    hangup: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

impl TerminationRequests {
    /// Starts listening for termination requests.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when a signal cannot be watched.
    pub fn watch() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};

            Ok(Self {
                interrupt: signal(SignalKind::interrupt())?,
                hangup: if hangup_ignored() {
                    None
                } else {
                    Some(signal(SignalKind::hangup())?)
                },
                terminate: signal(SignalKind::terminate())?,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {})
        }
    }

    /// Resolves once per request.
    pub async fn next(&mut self) {
        #[cfg(unix)]
        tokio::select! {
            Some(()) = self.interrupt.recv() => {}
            Some(()) = async {
                match self.hangup.as_mut() {
                    Some(hangup) => hangup.recv().await,
                    None => std::future::pending().await,
                }
            } => {}
            Some(()) = self.terminate.recv() => {}
            else => std::future::pending().await,
        }
        #[cfg(not(unix))]
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Whether the process was started with `SIGHUP` ignored.
///
/// Must be asked before anything registers the signal, because registering
/// replaces the disposition. Reads the process's own signal mask, which needs
/// no unsafe code.
#[cfg(unix)]
fn hangup_ignored() -> bool {
    std::fs::read_to_string("/proc/self/status").is_ok_and(|status| signal_ignored(&status, 1))
}

/// Whether `signal` is set in the `SigIgn` mask of a `/proc/<pid>/status` text.
#[cfg(unix)]
fn signal_ignored(status: &str, signal: u32) -> bool {
    status
        .lines()
        .find_map(|line| line.strip_prefix("SigIgn:"))
        .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
        .is_some_and(|mask| {
            signal
                .checked_sub(1)
                .is_some_and(|bit| mask >> bit & 1 == 1)
        })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_signal_started_as_ignored_is_found_in_the_process_mask() {
        let status = "Name:\tephervex\nSigBlk:\t0000000000000000\nSigIgn:\t0000000000000001\nSigCgt:\t0000000000004002\n";

        assert!(signal_ignored(status, 1));
        assert!(!signal_ignored(status, 2));
        assert!(!signal_ignored(status, 15));
        assert!(!signal_ignored("Name:\tephervex\n", 1));
    }
}
