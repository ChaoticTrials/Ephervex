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
pub mod pending_properties;
pub mod process;
pub mod properties;
pub mod property_catalog;
pub mod runtime;
pub mod statistics;
#[cfg(test)]
mod test_support;

pub use capture::{
    CAPTURE_BUDGET_BYTES, CAPTURE_INDEX_FILE, CAPTURE_SEGMENT_BYTES, CaptureHealth, CapturePrune,
    CaptureRetention, CaptureStream, CapturedLine, MAX_CAPTURE_LINE_BYTES, MAX_RING_BYTES,
    MAX_RING_LINES, OutputCapture, ServerOutputEvent, segment_continues_run,
};
pub use connection::wait_for_server_readiness;
pub use ephervex_model::ConnectionTarget;
pub use error::{ServerError, ServerResult};
pub use eula::{
    EULA_CONFIRMATION_PHRASE, EulaConsentRecord, EulaStatus, MINECRAFT_EULA_REFERENCE,
    accepted_record, materialize_eula_file, revoked_record,
};
pub use operators::{
    ListedOperator, MAX_LISTED_OPERATORS, OperatorEntry, OperatorListing, OperatorUpdate,
    OperatorsDocument, OperatorsDocumentError,
};
pub use pending_properties::{
    PENDING_PROPERTIES_FILE, PENDING_PROPERTIES_FORMAT_VERSION, PendingProperties, PropertiesLock,
    apply_pending_properties, pending_properties_path, read_properties_file, save_while_running,
};
pub use process::{
    ControlAction, ControlRequest, ControlResponse, ControlSocket, ServerCommand, ServerEvent,
    ServerEvents, ServerExit, ServerObserver, ServerSupervisor, ServerWarning, send_control,
    send_control_to,
};
pub use properties::{
    EULA_KEY, MalformedEscape, NetworkProperties, ONLINE_MODE_KEY, PropertiesDocument,
    SERVER_IP_KEY, SERVER_PORT_KEY, SERVER_PROPERTIES_FILE, ServerBind,
};
pub use property_catalog::{
    DIFFICULTIES, GAME_MODES, PropertyDefinition, PropertyGroup, PropertyValueKind,
    PropertyValueProblem, SERVER_PROPERTY_CATALOG, check_property_value, property_definition,
};
pub use runtime::{
    ProcessEvidence, ProcessRole, RUNTIME_FILE, RUNTIME_LOCK_FILE, RuntimeClassification,
    RuntimeEvidence, capture_java_child, classify_runtime, clear_stale_runtime,
    control_socket_path, read_runtime_evidence, runtime_path, validate_control_socket_directory,
    write_runtime_evidence,
};
pub use statistics::{PlayerRoster, ServerStatistics, bounded_display_name};
