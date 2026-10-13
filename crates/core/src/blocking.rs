//! Waiting on a lock without stalling the async runtime.
//!
//! The launch, ownership, and configuration locks wait for another thread or
//! process to let go, for up to several seconds. Waiting on one from an async
//! task holds a runtime worker, and every other task scheduled on it, for that
//! long. On a current-thread runtime that is the whole runtime.

/// Runs `work` on the blocking pool and waits for it without holding a worker.
///
/// Whatever `work` takes is released on the blocking thread, so a lock is
/// never carried across an await point: a task that held one while it was
/// descheduled would make a synchronous waiter on the same worker wait for a
/// holder that cannot run.
///
/// `work` runs inside the caller's tracing span, so what it logs still carries
/// the operation id and instance fields of the launch it belongs to.
///
/// A panic in `work` resumes here. `None` means the runtime shut down before
/// the work could run.
pub(crate) async fn off_worker<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let span = tracing::Span::current();
    match tokio::task::spawn_blocking(move || span.in_scope(work)).await {
        Ok(value) => Some(value),
        Err(error) => match error.try_into_panic() {
            Ok(payload) => std::panic::resume_unwind(payload),
            Err(_) => None,
        },
    }
}

/// Checks that `pending` waits without holding the runtime: other tasks on the
/// same runtime keep running while it is parked, and it has not finished.
///
/// A wait that held the worker would stall the ticker and run out its own
/// patience instead, finishing with a refusal before five ticks passed.
#[cfg(test)]
pub(crate) async fn assert_other_tasks_run_while<F>(pending: std::pin::Pin<&mut F>)
where
    F: std::future::Future,
    F::Output: std::fmt::Debug,
{
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let ticks = Arc::new(AtomicUsize::new(0));
    let ticker = tokio::spawn({
        let ticks = Arc::clone(&ticks);
        async move {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                ticks.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    tokio::select! {
        biased;
        outcome = pending => panic!("finished while its lock was held: {outcome:?}"),
        () = async {
            while ticks.load(Ordering::SeqCst) < 5 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        } => {}
    }
    ticker.abort();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[tokio::test]
    async fn work_runs_off_the_calling_thread_and_returns_its_value() {
        let caller = std::thread::current().id();

        let worker = off_worker(|| std::thread::current().id()).await;

        assert!(worker.is_some_and(|worker| worker != caller));
    }

    /// Records the threads that enter a span.
    struct EnteredOn(std::sync::Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for EnteredOn {
        fn on_enter(
            &self,
            _id: &tracing::span::Id,
            _context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.0.lock().unwrap().push(std::thread::current().id());
        }
    }

    #[tokio::test]
    async fn work_runs_inside_the_span_of_its_caller() {
        use tracing_subscriber::layer::SubscriberExt;

        let entered = std::sync::Arc::default();
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::registry().with(EnteredOn(std::sync::Arc::clone(&entered))),
        );
        let caller = std::thread::current().id();
        let span = tracing::info_span!("caller");

        {
            let _entered = span.enter();
            off_worker(|| ()).await;
        }

        assert!(
            entered
                .lock()
                .unwrap()
                .iter()
                .any(|thread| *thread != caller),
            "the span was entered on the blocking thread"
        );
    }

    #[tokio::test]
    async fn a_panic_in_the_work_resumes_in_the_caller() {
        let handle = tokio::spawn(off_worker(|| panic!("the work failed")));

        assert!(handle.await.expect_err("the task panicked").is_panic());
    }
}
