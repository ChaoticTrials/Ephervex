//! Ending the desktop application from outside its window.
//!
//! Closing the window ends the event loop, and the code after it releases what
//! the window held, such as the launcher's name for hand-offs. A termination
//! request (a session logout, `kill`, Ctrl-C in the starting terminal, a
//! closing terminal) would otherwise end the process without that code.

use ephervex_core::TerminationRequests;

/// Calls `on_request` on the first termination request.
///
/// Meant to ask the event loop to end, so the normal quit path runs. Later
/// requests are absorbed: the quit is already under way, and the code that
/// follows the event loop has not finished releasing what the window held.
/// A hangup is left alone when the process started with it ignored, as under
/// `nohup`.
///
/// The signals are registered before this returns, so a request that arrives
/// right after is not lost to the default disposition.
///
/// # Errors
///
/// Returns the operating system's error when the signals cannot be watched.
pub fn quit_on_termination_request(
    on_request: impl FnOnce() + Send + 'static,
) -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut requests = runtime.block_on(async { TerminationRequests::watch() })?;
    std::thread::Builder::new()
        .name("termination-requests".to_owned())
        .spawn(move || {
            runtime.block_on(requests.next());
            on_request();
            // The handlers outlive this thread, so later requests are absorbed.
        })?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    #[test]
    fn a_termination_request_calls_the_quit_hook_once_the_signals_are_watched() {
        let (sender, receiver) = std::sync::mpsc::channel();
        quit_on_termination_request(move || {
            let _ = sender.send(());
        })
        .expect("the signals can be watched");

        let status = std::process::Command::new("kill")
            .args(["-TERM", &std::process::id().to_string()])
            .status()
            .expect("kill runs");
        assert!(status.success());

        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the request reaches the hook");
    }
}
