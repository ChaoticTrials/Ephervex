use std::path::PathBuf;

/// Failures in dedicated-server installation and lifecycle services.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServerError {
    #[error("server property `{key}` has an invalid value: {reason}")]
    InvalidProperty { key: String, reason: &'static str },
    #[error("server property `{key}` is managed by a typed server setting")]
    ProtectedProperty { key: String },
    #[error("{path} has a malformed \\uXXXX escape on line {line}")]
    MalformedPropertiesEscape { path: PathBuf, line: usize },
    #[error("global Minecraft EULA consent has not been accepted")]
    EulaNotAccepted,
    #[error("the recorded Minecraft EULA consent is malformed")]
    EulaConsentMalformed,
    #[error("server runtime state at {path} could not be read or written")]
    RuntimeState {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("server file {path} could not be read or written")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("server runtime evidence cannot prove that the recorded process is gone")]
    RuntimeMayBeAlive,
    #[error("server control socket {path} is unavailable")]
    ControlUnavailable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("server command could not be started with {executable}")]
    ProcessStart {
        executable: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The old child stopped for a restart and its replacement could not be
    /// started, which is what ended the run.
    #[error("the server stopped for a restart and could not be started again")]
    RestartReplacementFailed {
        #[source]
        source: Box<ServerError>,
    },
    #[error("the server may already be running")]
    AlreadyRunning,
    #[error("the server console command must be one line and must not contain NUL")]
    InvalidConsoleCommand,
    #[error("server process I/O failed")]
    ProcessIo(#[source] std::io::Error),
    #[error("the server control channel returned an invalid response")]
    InvalidControlResponse,
    #[error("the server base is not installed")]
    NotInstalled,
    #[error("the server did not accept connections at {target} within {timeout_seconds} seconds")]
    ReadinessTimeout {
        target: String,
        timeout_seconds: u64,
    },
    #[error("waiting for the server was canceled")]
    ReadinessCanceled,
    #[error("starting the server was canceled")]
    StartCanceled,
    #[error("canonical server persistence is unhealthy: {reason}")]
    PersistenceUnhealthy { reason: &'static str },
}

impl ServerError {
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::InvalidProperty { .. } => "error-server-property-invalid",
            Self::ProtectedProperty { .. } => "error-server-property-protected",
            Self::MalformedPropertiesEscape { .. } => "error-server-properties-escape-malformed",
            Self::EulaNotAccepted => "error-server-eula-not-accepted",
            Self::EulaConsentMalformed => "error-server-eula-consent-malformed",
            Self::RuntimeState { .. } => "error-server-runtime-state",
            Self::Io { .. } => "error-server-file-io",
            Self::RuntimeMayBeAlive => "error-server-runtime-may-be-alive",
            Self::ControlUnavailable { .. } => "error-server-control-unavailable",
            Self::ProcessStart { .. } => "error-server-process-start",
            Self::RestartReplacementFailed { .. } => "error-server-restart-replacement-failed",
            Self::AlreadyRunning => "error-server-already-running",
            Self::InvalidConsoleCommand => "error-server-console-invalid",
            Self::ProcessIo(_) => "error-server-process-io",
            Self::InvalidControlResponse => "error-server-control-response-invalid",
            Self::NotInstalled => "error-server-not-installed",
            Self::ReadinessTimeout { .. } => "error-server-readiness-timeout",
            Self::ReadinessCanceled => "error-server-readiness-canceled",
            Self::StartCanceled => "error-server-start-canceled",
            Self::PersistenceUnhealthy { .. } => "error-server-persistence-unhealthy",
        }
    }

    #[must_use]
    pub fn message_args(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::ReadinessTimeout {
                target,
                timeout_seconds,
            } => vec![
                ("target", target.clone()),
                ("timeout_seconds", timeout_seconds.to_string()),
            ],
            Self::MalformedPropertiesEscape { line, .. } => vec![("line", line.to_string())],
            _ => Vec::new(),
        }
    }

    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::RuntimeState { path, .. }
            | Self::Io { path, .. }
            | Self::MalformedPropertiesEscape { path, .. }
            | Self::ControlUnavailable { path, .. } => Some(path.clone()),
            Self::ProcessStart { executable, .. } => Some(executable.clone()),
            Self::RestartReplacementFailed { source } => source.failing_path(),
            _ => None,
        }
    }
}

pub type ServerResult<T> = Result<T, ServerError>;
