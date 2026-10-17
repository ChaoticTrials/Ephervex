//! Helpers shared by this crate's unit tests.

#![allow(clippy::expect_used)]

use std::path::Path;
use std::process::Command;

/// Copies an executable from a `cp` process, so no fork in this test process can inherit the
/// copy's write descriptor and make executing it fail with `ETXTBSY`.
pub(crate) fn copy_executable(source: &Path, destination: &Path) {
    let status = Command::new("cp")
        .arg("--")
        .arg(source)
        .arg(destination)
        .status()
        .expect("run cp");
    assert!(status.success(), "cp exited with {status}");
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[test]
    fn a_copied_executable_runs_at_once_while_another_thread_keeps_forking() {
        let directory = tempfile::tempdir().expect("directory");
        let stop = Arc::new(AtomicBool::new(false));
        let forking = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::Relaxed) {
                    Command::new("true").status().expect("run true");
                }
            }
        });
        for index in 0..50 {
            let program = directory.path().join(format!("true-{index}"));
            copy_executable(Path::new("/bin/true"), &program);
            let status = Command::new(&program).status();
            assert!(
                status.as_ref().is_ok_and(std::process::ExitStatus::success),
                "copy {index} did not run: {status:?}"
            );
        }
        stop.store(true, Ordering::Relaxed);
        forking.join().expect("forking thread");
    }
}
