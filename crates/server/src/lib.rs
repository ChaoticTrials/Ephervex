//! Dedicated server installation, process management, console I/O, and EULA
//! consent enforcement.
//!
//! Frontends never call this crate directly. `ephervex-core` composes these
//! primitives with storage, loader installation, and Java selection.

pub mod capture;
mod connection;
mod error;
pub mod eula;
pub mod operators;
pub mod process;
pub mod properties;
pub mod runtime;

pub use capture::{
    CAPTURE_BUDGET_BYTES, CAPTURE_INDEX_FILE, CAPTURE_SEGMENT_BYTES, CaptureHealth,
    CaptureRetention, CaptureStream, CapturedLine, MAX_CAPTURE_LINE_BYTES, MAX_RING_BYTES,
    MAX_RING_LINES, OutputCapture, ServerOutputEvent,
};
pub use connection::wait_for_server_readiness;
pub use ephervex_model::ConnectionTarget;
pub use error::{ServerError, ServerResult};
pub use eula::{
    EULA_CONFIRMATION_PHRASE, EulaConsentRecord, EulaStatus, MINECRAFT_EULA_REFERENCE,
    accepted_record, materialize_eula_file, revoked_record,
};
pub use operators::{OperatorEntry, OperatorUpdate, OperatorsDocument, OperatorsDocumentError};
pub use process::{
    ControlAction, ControlResponse, ServerCommand, ServerEvent, ServerExit, ServerObserver,
    ServerSupervisor, ServerWarning, send_control,
};
pub use properties::{
    EULA_KEY, MalformedEscape, NetworkProperties, ONLINE_MODE_KEY, PropertiesDocument,
    SERVER_IP_KEY, SERVER_PORT_KEY, ServerBind,
};
pub use runtime::{
    ProcessEvidence, ProcessRole, RUNTIME_FILE, RUNTIME_LOCK_FILE, RuntimeClassification,
    RuntimeEvidence, capture_java_child, classify_runtime, clear_stale_runtime,
    control_socket_path, read_runtime_evidence, runtime_path, write_runtime_evidence,
};
