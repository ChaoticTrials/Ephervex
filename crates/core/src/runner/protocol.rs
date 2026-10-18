//! What the starting process and a runner say to each other.
//!
//! The starting process writes one [`RunnerSpec`] to the runner's standard
//! input and closes it, so nothing the specification carries, such as an access
//! token inside a game's arguments, touches the disk. The runner answers on its
//! standard output with one JSON message per line.

use std::path::PathBuf;

use ephervex_minecraft::{CommandHandOff, LogRecord, OutputStream};
use ephervex_server::{
    CaptureRetention, CapturedLine, ServerCommand, ServerError, ServerEvent, ServerExit,
};
use ephervex_storage::LaunchState;
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
    Client(Box<ClientRunnerSpec>),
}

/// A prepared client launch, handed to the runner that supervises its game.
///
/// Holds the complete command, access token included, so it only ever travels
/// over the runner's standard input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ClientRunnerSpec {
    pub command: CommandHandOff,
    pub instance_root: PathBuf,
    /// The handle of an additional launch, or `None` for the instance's first.
    pub additional: Option<String>,
    /// The lock the starter holds, which the runner takes over.
    pub previous: LaunchState,
    /// The run id the game carries in its environment.
    pub run: uuid::Uuid,
    pub capture_directory: PathBuf,
    /// The shared artifact cache the game's classpath names. The runner holds
    /// the cache's lease for as long as the game runs.
    pub cache_directory: PathBuf,
}

/// A request the starter makes of a runner after handing it its command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "request")]
pub(crate) enum RunnerRequest {
    /// End the game and everything it started.
    Kill,
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

/// Which pipe a hosted game's line arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WireStream {
    Stdout,
    Stderr,
}

impl From<OutputStream> for WireStream {
    fn from(stream: OutputStream) -> Self {
        match stream {
            OutputStream::Stdout => Self::Stdout,
            OutputStream::Stderr => Self::Stderr,
        }
    }
}

impl From<WireStream> for OutputStream {
    fn from(stream: WireStream) -> Self {
        match stream {
            WireStream::Stdout => Self::Stdout,
            WireStream::Stderr => Self::Stderr,
        }
    }
}

/// One parsed line of a hosted game's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WireRecord {
    pub level: Option<String>,
    pub logger: Option<String>,
    pub thread: Option<String>,
    pub message: String,
}

impl From<&LogRecord> for WireRecord {
    fn from(record: &LogRecord) -> Self {
        Self {
            level: record.level.clone(),
            logger: record.logger.clone(),
            thread: record.thread.clone(),
            message: record.message.clone(),
        }
    }
}

impl From<WireRecord> for LogRecord {
    fn from(record: WireRecord) -> Self {
        Self {
            level: record.level,
            logger: record.logger,
            thread: record.thread,
            message: record.message,
        }
    }
}

/// How a hosted game ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GameEnd {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    /// What each stream printed first, for explaining a game that could not start.
    pub early_output: Vec<String>,
}

/// Why a hosted launch failed before or while the game ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "failure", content = "detail")]
pub(crate) enum HostedLaunchFailure {
    /// The lock no longer named the starter, so the runner could not take it over.
    LockLost {
        path: PathBuf,
    },
    ProcessStart {
        executable: PathBuf,
        cause: String,
    },
    GameDirectory {
        path: PathBuf,
        cause: String,
    },
    Other(String),
}

/// One message from a runner that hosts a client's game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "message")]
pub(crate) enum ClientRunnerMessage {
    /// The runner owns the launch's lock and the game is running.
    Running {
        pid: u32,
    },
    Output {
        stream: WireStream,
        record: WireRecord,
    },
    OutputDropped {
        lines: u64,
    },
    /// The run is over. Always the last message.
    Finished {
        result: Result<GameEnd, HostedLaunchFailure>,
    },
}
