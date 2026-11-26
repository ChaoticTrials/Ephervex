//! Controlling a client launch that a runner owns, from any Ephervex process.

use ephervex_model::InstanceId;
use ephervex_server::{
    ControlAction, ControlResponse, ServerError, send_control_to, validate_control_socket_directory,
};
use ephervex_storage::{FIRST_LAUNCH_HANDLE, RunningLaunch};

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
    /// The process named runs more than one of the instance's launches, as a
    /// launcher running several of them itself does. These are their handles,
    /// any of which names one launch alone.
    Ambiguous { handles: Vec<String> },
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

/// Which of an instance's launches a request is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchSelector {
    /// The first launch, which holds the unnamed lock.
    First,
    /// An additional launch, by the handle its starter gave it.
    Handle(String),
    /// The launch whose game or launcher run is this process.
    Process(u32),
}

impl LaunchSelector {
    /// Reads what a user typed to name a launch.
    ///
    /// [`FIRST_LAUNCH_HANDLE`] is the first launch, and a number is a process
    /// id, because a process id is what a user has at hand and what a list of
    /// running processes shows. No new launch can be given either as its
    /// handle, so neither can name one launch while meaning another.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        if text == FIRST_LAUNCH_HANDLE {
            return Self::First;
        }
        if !text.is_empty()
            && text.bytes().all(|byte| byte.is_ascii_digit())
            && let Ok(pid) = text.parse()
        {
            return Self::Process(pid);
        }
        Self::Handle(text.to_owned())
    }

    /// The launch a handle read from a running launch names, exactly: `None` is
    /// the first launch, and any other handle is matched as a name.
    #[must_use]
    pub fn of_handle(handle: Option<&str>) -> Self {
        handle.map_or(Self::First, |handle| Self::Handle(handle.to_owned()))
    }

    /// Whether `running` is the launch this names.
    ///
    /// A process matches only while it is proven to still be that launch's
    /// game or launcher run, so a recorded pid the system has since given to
    /// something else never matches. A launch an earlier version let a user
    /// name with that same number matches too, so typing a name it once had
    /// is refused as ambiguous instead of reaching the other launch.
    fn matches(&self, running: &RunningLaunch) -> bool {
        match self {
            Self::First => running.id.is_none(),
            Self::Handle(handle) => running.id.as_deref() == Some(handle.as_str()),
            Self::Process(pid) => {
                let state = &running.state;
                (state.game_pid == Some(*pid) && state.game_is_alive())
                    || (state.owner_pid == *pid && state.owner_is_alive())
                    || running.id.as_deref() == Some(pid.to_string().as_str())
            }
        }
    }
}

/// The text that names `running` where a user is asked which launch, which
/// [`LaunchSelector::parse`] reads back as that launch.
///
/// That is its handle, or [`FIRST_LAUNCH_HANDLE`] for the first launch. A
/// launch an earlier version let a user name with digits alone or with
/// `first` is named by its live process instead, since its own name would be
/// read as another launch.
pub(crate) fn launch_handle(running: &RunningLaunch) -> String {
    match running.id.as_deref() {
        None => FIRST_LAUNCH_HANDLE.to_owned(),
        Some(handle)
            if LaunchSelector::parse(handle) == LaunchSelector::Handle(handle.to_owned()) =>
        {
            handle.to_owned()
        }
        Some(_) => {
            let state = &running.state;
            match state.game_pid {
                Some(game) if state.game_is_alive() => game,
                _ => state.owner_pid,
            }
            .to_string()
        }
    }
}

impl App {
    /// Asks the runner that owns a client launch to end the game.
    ///
    /// `launch` names which of the instance's launches, for an instance running
    /// more than one. The runner is addressed through the socket the launch's
    /// lock records, after its process is proven by its recorded start time and
    /// the socket's peer is checked to be that process.
    pub async fn stop_hosted_launch(
        &self,
        instance: InstanceId,
        launch: &LaunchSelector,
        how: HostedStop,
    ) -> CoreResult<HostedStopOutcome> {
        let root = self.client(instance)?.root;
        let mut matching: Vec<RunningLaunch> = ephervex_storage::running_launches(&root)
            .into_iter()
            .filter(|running| launch.matches(running))
            .collect();
        if matching.len() > 1 {
            let handles = matching.iter().map(launch_handle).collect();
            return Ok(HostedStopOutcome::Ambiguous { handles });
        }
        let Some(running) = matching.pop() else {
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

    /// Writes a lock describing a launch `owner` owns, under `handle`, the way
    /// a launcher records one. `None` is the first launch.
    fn record_launch(root: &std::path::Path, handle: Option<&str>, owner: u32) {
        let directory = root.join(ephervex_storage::paths::LAUNCHER_METADATA_DIR);
        std::fs::create_dir_all(&directory).expect("metadata directory");
        let file = handle.map_or_else(
            || ephervex_storage::LAUNCH_LOCK_FILE.to_owned(),
            |handle| format!("launch.{handle}.lock"),
        );
        let state = ephervex_storage::LaunchState {
            owner_pid: owner,
            owner_start_ticks: None,
            game_pid: None,
            game_start_ticks: None,
            since: chrono::Utc::now(),
            runner: None,
        };
        std::fs::write(
            directory.join(file),
            serde_json::to_vec(&state).expect("state serializes"),
        )
        .expect("lock written");
    }

    fn sleeper() -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("a process to own a launch")
    }

    #[test]
    fn what_a_user_types_names_the_first_launch_a_process_or_a_handle() {
        assert_eq!(LaunchSelector::parse("first"), LaunchSelector::First);
        assert_eq!(
            LaunchSelector::parse("239745"),
            LaunchSelector::Process(239_745)
        );
        assert_eq!(
            LaunchSelector::parse("second"),
            LaunchSelector::Handle("second".to_owned())
        );
        assert_eq!(
            LaunchSelector::parse("4242-1"),
            LaunchSelector::Handle("4242-1".to_owned())
        );
        // Too large to be a process id, so it can only be a name.
        assert_eq!(
            LaunchSelector::parse("99999999999"),
            LaunchSelector::Handle("99999999999".to_owned())
        );
        assert_eq!(LaunchSelector::of_handle(None), LaunchSelector::First);
        assert_eq!(
            LaunchSelector::of_handle(Some("239745")),
            LaunchSelector::Handle("239745".to_owned())
        );
    }

    /// A launch an earlier version let a user name after another launch's
    /// process id must not be stopped in place of that process's launch, nor
    /// the other way round, and is listed under a name that reaches it alone.
    #[tokio::test]
    async fn a_process_id_never_silently_reaches_a_launch_other_than_its_own() {
        let (_base, app, client) = app_with_client();
        let mut first = sleeper();
        let mut second = sleeper();
        let first_pid = first.id();
        record_launch(&client.root, None, first_pid);
        record_launch(&client.root, Some(&first_pid.to_string()), second.id());

        let running = app.running_launches(client.id).expect("listed");
        let chosen: Vec<_> = running
            .iter()
            .filter(|running| LaunchSelector::parse(&first_pid.to_string()).matches(running))
            .map(|running| running.id.clone())
            .collect();
        let named: Vec<_> = running
            .iter()
            .filter(|running| LaunchSelector::parse(&second.id().to_string()).matches(running))
            .map(|running| running.id.clone())
            .collect();

        let shown: Vec<String> = running.iter().map(launch_handle).collect();
        let ambiguous = app
            .stop_hosted_launch(
                client.id,
                &LaunchSelector::parse(&first_pid.to_string()),
                HostedStop::Kill,
            )
            .await
            .expect("asked");

        first.kill().expect("first ends");
        second.kill().expect("second ends");
        let _ = first.wait();
        let _ = second.wait();
        assert_eq!(running.len(), 2, "both launches are running");
        assert_eq!(
            chosen,
            vec![None, Some(first_pid.to_string())],
            "the first launch's pid reaches the first launch, and the launch once named after it"
        );
        // Each launch is listed as text that names it alone.
        assert_eq!(shown, vec!["first".to_owned(), second.id().to_string()]);
        assert_eq!(
            ambiguous,
            HostedStopOutcome::Ambiguous { handles: shown },
            "a number that is one launch's pid and another's old name stops neither"
        );
        assert_eq!(
            named,
            vec![Some(first_pid.to_string())],
            "the second launch's pid names the second launch"
        );
    }

    #[tokio::test]
    async fn a_process_that_runs_several_launches_names_none_of_them_alone() {
        let (_base, app, client) = app_with_client();
        let mut owner = sleeper();
        record_launch(&client.root, None, owner.id());
        record_launch(&client.root, Some("second"), owner.id());

        let outcome = app
            .stop_hosted_launch(
                client.id,
                &LaunchSelector::Process(owner.id()),
                HostedStop::Kill,
            )
            .await
            .expect("asked");

        owner.kill().expect("owner ends");
        let _ = owner.wait();
        assert_eq!(
            outcome,
            HostedStopOutcome::Ambiguous {
                handles: vec!["first".to_owned(), "second".to_owned()]
            }
        );
    }

    #[tokio::test]
    async fn an_instance_with_no_launch_reports_that_nothing_is_running() {
        let (_base, app, client) = app_with_client();

        let outcome = app
            .stop_hosted_launch(client.id, &LaunchSelector::First, HostedStop::Kill)
            .await
            .expect("asked");

        assert_eq!(outcome, HostedStopOutcome::NotRunning);
    }

    #[tokio::test]
    async fn a_launch_no_runner_owns_has_no_channel_to_ask() {
        let (_base, app, client) = app_with_client();
        let _lock = LaunchLock::acquire(&client.root).expect("a plain launch");

        let outcome = app
            .stop_hosted_launch(client.id, &LaunchSelector::First, HostedStop::Kill)
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
            paths: app.paths().clone(),
            instance: client.id,
            started: chrono::Utc::now(),
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
            .stop_hosted_launch(client.id, &LaunchSelector::First, HostedStop::Kill)
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
