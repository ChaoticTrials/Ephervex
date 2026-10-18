//! The runner side of a hosted server: the supervisor that outlives the process
//! which started it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ephervex_server::{
    ControlAction, ServerError, ServerEvents, ServerObserver, ServerSupervisor,
    read_runtime_evidence, send_control,
};
use tokio::io::AsyncWrite;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;

use super::protocol::{HostedFailure, ServerRunnerMessage, ServerRunnerSpec};
use super::relay::Relay;
use crate::TerminationRequests;

/// How long a termination request waits for the supervisor's control channel to
/// be published before it stops asking.
const CONTROL_PUBLICATION_PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

/// Reports to the relay that the runner has settled who owns the server's
/// runtime lock, on every path out of taking it.
struct Settled(Option<oneshot::Sender<()>>);

impl Drop for Settled {
    fn drop(&mut self) {
        if let Some(settled) = self.0.take() {
            let _ = settled.send(());
        }
    }
}

/// Supervises one server for as long as it runs, reporting to `out`.
pub(crate) async fn host_server<W>(spec: ServerRunnerSpec, out: W) -> u8
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    // Watched before the supervisor starts, so a request that arrives while the
    // server is starting is not lost to the default disposition.
    let requests = match TerminationRequests::watch() {
        Ok(requests) => Some(super::termination_channel(requests)),
        Err(error) => {
            tracing::warn!(%error, "termination requests are not watched by the runner");
            None
        }
    };
    host_server_until(spec, out, requests).await
}

/// Supervises one server, treating each message on `terminations` as one
/// termination request.
pub(crate) async fn host_server_until<W>(
    spec: ServerRunnerSpec,
    out: W,
    terminations: Option<mpsc::UnboundedReceiver<()>>,
) -> u8
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (observer, events) = ServerObserver::channel();
    let (settled, settled_receiver) = oneshot::channel();
    let relay = tokio::spawn(relay_events(events, settled_receiver, Relay::new(out)));
    let signals = terminations.map(|requests| {
        tokio::spawn(forward_termination(
            requests,
            Arc::new(spec.server_root.clone()),
        ))
    });

    let result = ServerSupervisor::run_holding(
        Settled(Some(settled)),
        &spec.command,
        &spec.server_root,
        &spec.capture_directory,
        &observer,
    )
    .await;
    drop(observer);
    if let Some(signals) = signals {
        signals.abort();
    }
    let relayed = relay.await;
    let succeeded = result.is_ok();
    if let Ok(mut relay) = relayed {
        relay
            .send(&ServerRunnerMessage::Finished {
                result: result.map_err(|error| HostedFailure::from(&error)),
            })
            .await;
    }
    tokio::time::sleep(super::CONTROL_ANSWER_GRACE).await;
    u8::from(!succeeded)
}

/// Writes the supervisor's events as messages until its observer is gone.
async fn relay_events<W>(
    mut events: ServerEvents,
    mut settled: oneshot::Receiver<()>,
    mut relay: Relay<W>,
) -> Relay<W>
where
    W: AsyncWrite + Unpin,
{
    let mut waiting_for_settled = true;
    loop {
        tokio::select! {
            biased;
            _ = &mut settled, if waiting_for_settled => {
                waiting_for_settled = false;
                relay.send(&ServerRunnerMessage::Settled).await;
            }
            event = events.recv() => {
                let Some(event) = event else { break };
                if let Some(message) = ServerRunnerMessage::from_event(event) {
                    relay.send(&message).await;
                }
            }
        }
    }
    if waiting_for_settled && settled.await.is_ok() {
        relay.send(&ServerRunnerMessage::Settled).await;
    }
    relay
}

/// Turns the runner's termination requests into the supervisor's: the first
/// stops the server gracefully and a later one kills it.
///
/// Requests only ever reach this runner's own run. Every send is owned by this
/// task, so ending the task ends the sends still waiting to be delivered.
async fn forward_termination(mut requests: mpsc::UnboundedReceiver<()>, root: Arc<PathBuf>) {
    let mut asked = 0_u32;
    let mut deliveries = JoinSet::new();
    while requests.recv().await.is_some() {
        asked += 1;
        let action = if asked == 1 {
            ControlAction::Stop
        } else {
            ControlAction::Kill
        };
        let root = Arc::clone(&root);
        deliveries.spawn(async move { deliver(&root, &action).await });
    }
    // The requests have ended; what was asked is still delivered.
    while deliveries.join_next().await.is_some() {}
}

/// Sends `action` to this runner's supervisor, retrying while it is still
/// publishing its channel.
///
/// The channel is addressed through the runtime evidence, which names whichever
/// supervisor owns the server. Nothing is sent unless that is this process, so
/// a request never reaches a server another process supervises.
async fn deliver(root: &Path, action: &ControlAction) {
    let deadline = tokio::time::Instant::now() + CONTROL_PUBLICATION_PATIENCE;
    loop {
        let ours = matches!(
            read_runtime_evidence(root),
            Ok(Some(evidence)) if evidence.supervisor.pid == std::process::id()
        );
        if ours {
            match send_control(root, action).await {
                Err(ServerError::ControlUnavailable { source, .. })
                    if source.kind() != std::io::ErrorKind::TimedOut => {}
                _ => return,
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}
