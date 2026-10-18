//! What the starting process and a runner say to each other.
//!
//! The starting process writes one [`RunnerSpec`] to the runner's standard
//! input and closes it, so nothing the specification carries, such as an access
//! token inside a game's arguments, touches the disk. The runner answers on its
//! standard output with one JSON message per line.

use std::path::PathBuf;

use ephervex_server::{
    CaptureRetention, CapturedLine, ServerCommand, ServerError, ServerEvent, ServerExit,
};
use serde::{Deserialize, Serialize};

/// The only argument a frontend passes to start a runner.
///
/// Checked before any other argument handling, so it never reaches command-line
/// parsing and never appears in `--help`.
pub const RUNNER_ARGUMENT: &str = "--ephervex-runner";

/// The longest line either side accepts, which bounds what a peer can make the
/// other buffer. A captured line is at most 64 KiB, and JSON escaping at most
/// sextuples it.
pub(crate) const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// What a runner is asked to host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "host")]
pub(crate) enum RunnerSpec {
    Server(ServerRunnerSpec),
}

/// A prepared server start, handed to the runner that supervises it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ServerRunnerSpec {
    pub command: ServerCommand,
    pub server_root: PathBuf,
    pub capture_directory: PathBuf,
}

/// Why a hosted supervision failed, as far as the starter distinguishes it.
///
/// A failure that has a message of its own keeps its kind, so the user reads
/// the same explanation and the same advice as for an attached run. The paths
/// and the operating system's wording travel as text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "failure", content = "detail")]
pub(crate) enum HostedFailure {
    AlreadyRunning,
    RuntimeMayBeAlive,
    RestartReplacementFailed(Box<HostedFailure>),
    ProcessStart { executable: PathBuf, cause: String },
    RuntimeState { path: PathBuf, cause: String },
    Io { path: PathBuf, cause: String },
    ControlUnavailable { path: PathBuf, cause: String },
    ProcessIo(String),
    Other(String),
}

impl From<&ServerError> for HostedFailure {
    fn from(error: &ServerError) -> Self {
        use std::error::Error as _;
        let cause = || error.source().map(ToString::to_string).unwrap_or_default();
        match error {
            ServerError::AlreadyRunning => Self::AlreadyRunning,
            ServerError::RuntimeMayBeAlive => Self::RuntimeMayBeAlive,
            ServerError::RestartReplacementFailed { source } => {
                Self::RestartReplacementFailed(Box::new(Self::from(source.as_ref())))
            }
            ServerError::ProcessStart { executable, .. } => Self::ProcessStart {
                executable: executable.clone(),
                cause: cause(),
            },
            ServerError::RuntimeState { path, .. } => Self::RuntimeState {
                path: path.clone(),
                cause: cause(),
            },
            ServerError::Io { path, .. } => Self::Io {
                path: path.clone(),
                cause: cause(),
            },
            ServerError::ControlUnavailable { path, .. } => Self::ControlUnavailable {
                path: path.clone(),
                cause: cause(),
            },
            ServerError::ProcessIo(_) => Self::ProcessIo(cause()),
            other => Self::Other(crate::ErrorChain(other).to_string()),
        }
    }
}

impl From<HostedFailure> for ServerError {
    fn from(failure: HostedFailure) -> Self {
        let source = |cause: String| std::io::Error::other(cause);
        match failure {
            HostedFailure::AlreadyRunning => Self::AlreadyRunning,
            HostedFailure::RuntimeMayBeAlive => Self::RuntimeMayBeAlive,
            HostedFailure::RestartReplacementFailed(inner) => Self::RestartReplacementFailed {
                source: Box::new(Self::from(*inner)),
            },
            HostedFailure::ProcessStart { executable, cause } => Self::ProcessStart {
                executable,
                source: source(cause),
            },
            HostedFailure::RuntimeState { path, cause } => Self::RuntimeState {
                path,
                source: source(cause),
            },
            HostedFailure::Io { path, cause } => Self::Io {
                path,
                source: source(cause),
            },
            HostedFailure::ControlUnavailable { path, cause } => Self::ControlUnavailable {
                path,
                source: source(cause),
            },
            HostedFailure::ProcessIo(cause) => Self::ProcessIo(source(cause)),
            HostedFailure::Other(detail) => Self::HostedRun { detail },
        }
    }
}

/// One message from a runner that hosts a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "message")]
pub(crate) enum ServerRunnerMessage {
    /// The runner owns the server's runtime lock, or has given up trying, which
    /// is the point at which the starter's own claim on the server has done its
    /// work.
    Settled,
    Started {
        pid: u32,
    },
    Output {
        line: CapturedLine,
    },
    OutputDropped {
        lines: u64,
    },
    CapturePruned {
        retention: CaptureRetention,
        current_run_pruned: bool,
    },
    CaptureUnavailable {
        detail: String,
    },
    Exited {
        exit: ServerExit,
    },
    Restarting,
    /// The supervised run is over. Always the last message.
    Finished {
        result: Result<ServerExit, HostedFailure>,
    },
}

impl ServerRunnerMessage {
    /// The message for an event the runner's supervisor emitted, or `None` for
    /// an event only the starting process produces.
    pub(crate) fn from_event(event: ServerEvent) -> Option<Self> {
        Some(match event {
            ServerEvent::Started { pid } => Self::Started { pid },
            ServerEvent::Output(line) => Self::Output { line },
            ServerEvent::OutputDropped { lines } => Self::OutputDropped { lines },
            ServerEvent::CapturePruned(prune) => Self::CapturePruned {
                retention: prune.retention,
                current_run_pruned: prune.current_run_pruned,
            },
            ServerEvent::CaptureUnavailable { detail } => Self::CaptureUnavailable { detail },
            ServerEvent::Exited(exit) => Self::Exited { exit },
            ServerEvent::Restarting => Self::Restarting,
            _ => return None,
        })
    }

    /// The event this message stands for in the starting process.
    pub(crate) fn into_event(self) -> Option<ServerEvent> {
        Some(match self {
            Self::Started { pid } => ServerEvent::Started { pid },
            Self::Output { line } => ServerEvent::Output(line),
            Self::OutputDropped { lines } => ServerEvent::OutputDropped { lines },
            Self::CapturePruned {
                retention,
                current_run_pruned,
            } => ServerEvent::CapturePruned(ephervex_server::CapturePrune {
                retention,
                current_run_pruned,
            }),
            Self::CaptureUnavailable { detail } => ServerEvent::CaptureUnavailable { detail },
            Self::Exited { exit } => ServerEvent::Exited(exit),
            Self::Restarting => ServerEvent::Restarting,
            Self::Settled | Self::Finished { .. } => return None,
        })
    }
}
