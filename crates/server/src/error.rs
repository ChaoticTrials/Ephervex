use std::path::PathBuf;

/// Failures in dedicated-server installation and lifecycle services.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServerError {
    #[error("server property `{key}` has an invalid value: {reason}")]
    InvalidProperty { key: String, reason: &'static str },
    #[error("server property `{key}` is managed by a typed server setting")]
    ProtectedProperty { key: String },
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
    #[error("canonical server persistence is unhealthy: {reason}")]
    PersistenceUnhealthy { reason: &'static str },
}

impl ServerError {
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::InvalidProperty { .. } => "error-server-property-invalid",
            Self::ProtectedProperty { .. } => "error-server-property-protected",
            Self::EulaNotAccepted => "error-server-eula-not-accepted",
            Self::EulaConsentMalformed => "error-server-eula-consent-malformed",
            Self::RuntimeState { .. } => "error-server-runtime-state",
            Self::Io { .. } => "error-server-file-io",
            Self::RuntimeMayBeAlive => "error-server-runtime-may-be-alive",
            Self::ControlUnavailable { .. } => "error-server-control-unavailable",
            Self::ProcessStart { .. } => "error-server-process-start",
            Self::AlreadyRunning => "error-server-already-running",
            Self::InvalidConsoleCommand => "error-server-console-invalid",
            Self::ProcessIo(_) => "error-server-process-io",
            Self::InvalidControlResponse => "error-server-control-response-invalid",
            Self::NotInstalled => "error-server-not-installed",
            Self::PersistenceUnhealthy { .. } => "error-server-persistence-unhealthy",
        }
    }

    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::RuntimeState { path, .. }
            | Self::Io { path, .. }
            | Self::ControlUnavailable { path, .. } => Some(path.clone()),
            Self::ProcessStart { executable, .. } => Some(executable.clone()),
            _ => None,
        }
    }
}

pub type ServerResult<T> = Result<T, ServerError>;
