//! Running blocking archive and filesystem work away from async workers.

use crate::error::{PackError, PackResult};

/// Runs archive and filesystem work on a blocking thread.
///
/// The ZIP reader and recursive copies are blocking, so doing them on an async
/// worker would stall every other task on it for the whole operation. A panic
/// in `work` is [`PackError::WorkerFailed`], never a cancellation.
pub(crate) async fn off_runtime<T: Send + 'static>(
    work: impl FnOnce() -> PackResult<T> + Send + 'static,
) -> PackResult<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(failure) if failure.is_panic() => Err(PackError::WorkerFailed),
        Err(_) => Err(PackError::Canceled),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[tokio::test]
    async fn a_panic_in_blocking_work_is_a_worker_failure_not_a_cancellation() {
        let error = off_runtime::<()>(|| panic!("extraction crashed"))
            .await
            .expect_err("a panic fails the operation");
        assert!(matches!(error, PackError::WorkerFailed));
        assert!(!error.is_cancellation());
    }

    #[tokio::test]
    async fn blocking_work_returns_its_own_result() {
        assert_eq!(off_runtime(|| Ok(7)).await.expect("ok"), 7);
    }

    #[tokio::test]
    async fn blocking_work_runs_while_another_task_gets_the_only_worker() {
        // A current-thread runtime has one worker, so the other task can only
        // run once the awaiting task is suspended. The work waits for it, so
        // work run on the worker itself would time out instead of finishing.
        // Waiting on a channel keeps the outcome independent of how quickly
        // the blocking thread runs.
        let (sender, receiver) = std::sync::mpsc::channel::<()>();
        let other_task = tokio::spawn(async move {
            sender.send(()).expect("the work is still waiting");
        });

        let heard = off_runtime(move || {
            Ok(receiver
                .recv_timeout(std::time::Duration::from_secs(30))
                .is_ok())
        })
        .await
        .expect("ok");

        other_task.await.expect("the other task finished");
        assert!(heard, "another task never ran while the work was waiting");
    }
}
