//! Controlling a client launch that a runner owns, from any Ephervex process.

use ephervex_model::InstanceId;
use ephervex_server::{
    ControlAction, ControlResponse, ServerError, send_control_to, validate_control_socket_directory,
};

use crate::app::App;
use crate::error::CoreResult;

/// How a hosted client's game is asked to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedStop {
    /// Ask the game's processes to terminate, as closing a window would, and
    /// leave them to finish. Never escalates.
    Graceful,
    /// End the game and everything it started at once.
    Kill,
}

/// What stopping a hosted launch found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostedStopOutcome {
    /// The instance holds no such launch.
    NotRunning,
    /// The launch is not owned by a runner that can be proven to still be the
    /// recorded process, so there is no channel to ask. A launch whose runner is
    /// gone is an orphan, and only the validated force stop reaches it.
    NotHosted,
    /// The runner answered.
    Answered(ControlResponse),
    /// The runner accepted the request, and the game had not exited when the
    /// wait for it ended. It may still be running.
    NotEnded,
}

impl App {
    /// Asks the runner that owns a client launch to end the game.
    ///
    /// `launch` names which of the instance's launches, for an instance running
    /// more than one; `None` is the first. The runner is addressed through the
    /// socket the launch's lock records, after its process is proven by its
    /// recorded start time and the socket's peer is checked to be that process.
    pub async fn stop_hosted_launch(
        &self,
        instance: InstanceId,
        launch: Option<&str>,
        how: HostedStop,
    ) -> CoreResult<HostedStopOutcome> {
        let root = self.client(instance)?.root;
        let Some(running) = ephervex_storage::running_launches(&root)
            .into_iter()
            .find(|running| running.id.as_deref() == launch)
        else {
            return Ok(HostedStopOutcome::NotRunning);
        };
        let state = running.state;
        let Some(runner) = state.runner.as_ref().filter(|_| state.runner_is_proven()) else {
            return Ok(HostedStopOutcome::NotHosted);
        };
        validate_control_socket_directory(&runner.control_socket)?;
        let action = match how {
            HostedStop::Graceful => ControlAction::Stop,
            HostedStop::Kill => ControlAction::Kill,
        };
        match send_control_to(&runner.control_socket, &action, Some(state.owner_pid)).await {
            Ok(response) => Ok(HostedStopOutcome::Answered(response)),
            Err(ServerError::ControlUnavailable { source, .. })
                if source.kind() == std::io::ErrorKind::TimedOut =>
            {
                Ok(HostedStopOutcome::NotEnded)
            }
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::{LoaderSelection, StorageClass};
    use ephervex_storage::{LaunchLock, NewClient, Paths};
    use tokio::sync::mpsc;

    use super::*;
    use crate::runner::{ClientRunnerSpec, host_client, shell_game};

    fn app_with_client() -> (tempfile::TempDir, App, ephervex_storage::ClientRecord) {
        let base = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(Paths::rooted_at(base.path())).expect("app opens");
        let client = app
            .create_client_for_test(
                NewClient::new("Hosted", "1.20.1", LoaderSelection::vanilla())
                    .with_storage(StorageClass::Temporary),
            )
            .expect("create");
        (base, app, client)
    }

    #[tokio::test]
    async fn an_instance_with_no_launch_reports_that_nothing_is_running() {
        let (_base, app, client) = app_with_client();

        let outcome = app
            .stop_hosted_launch(client.id, None, HostedStop::Kill)
            .await
            .expect("asked");

        assert_eq!(outcome, HostedStopOutcome::NotRunning);
    }

    #[tokio::test]
    async fn a_launch_no_runner_owns_has_no_channel_to_ask() {
        let (_base, app, client) = app_with_client();
        let _lock = LaunchLock::acquire(&client.root).expect("a plain launch");

        let outcome = app
            .stop_hosted_launch(client.id, None, HostedStop::Kill)
            .await
            .expect("asked");

        assert_eq!(outcome, HostedStopOutcome::NotHosted);
    }

    #[tokio::test]
    async fn a_hosted_launch_is_ended_by_a_later_process_through_the_instance_alone() {
        let (_base, app, client) = app_with_client();
        let starter = LaunchLock::acquire(&client.root).expect("the starter's lock");
        let game_directory = client.root.join("minecraft");
        std::fs::create_dir_all(&game_directory).expect("game directory");
        let spec = ClientRunnerSpec {
            command: shell_game("sleep 30", &game_directory),
            instance_root: client.root.clone(),
            additional: None,
            previous: starter.state().clone(),
            run: uuid::Uuid::new_v4(),
            capture_directory: client.root.join("capture"),
            cache_directory: client.root.join("cache"),
        };
        let (_reports, writer) = tokio::io::duplex(64 * 1024);
        let (_requests, request_receiver) = mpsc::unbounded_channel();
        let host = tokio::spawn(host_client(spec, writer, request_receiver, None));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while ephervex_storage::running_state(&client.root)
                .is_none_or(|state| state.game_pid.is_none())
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the game starts");
        // The starter's handle is another process in a real launch.
        std::mem::forget(starter);

        let outcome = app
            .stop_hosted_launch(client.id, None, HostedStop::Kill)
            .await
            .expect("asked");

        let HostedStopOutcome::Answered(ControlResponse::Exited(exit)) = outcome else {
            panic!("expected the game's exit, got {outcome:?}");
        };
        assert_eq!(exit.signal, Some(9));
        assert!(ephervex_storage::running_state(&client.root).is_none());
        assert_eq!(host.await.expect("host task"), 0);
    }
}
