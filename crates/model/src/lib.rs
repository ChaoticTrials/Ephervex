//! Domain model shared by every Ephervex crate.
//!
//! This crate is the root of the dependency graph: it depends on no other
//! workspace crate, and every other crate may depend on it. It holds types
//! only — no networking, no filesystem access, no business logic.

pub mod account;
pub mod app;
pub mod hash;
pub mod ids;
pub mod instance;
pub mod java;
pub mod loader;
pub mod mods;
pub mod operation;
pub mod server;
pub mod storage;

pub use account::AccountKind;
pub use app::{APP_ID, APP_NAME, APP_URL, APP_VERSION, user_agent};
pub use hash::{Hash, HashAlgorithm};
pub use ids::{InstanceId, OperationId, ServerId};
pub use instance::{AttentionReason, ImportSourceKind, InstanceKind};
pub use java::{JavaOverride, JvmSettings};
pub use loader::{LoaderKind, LoaderSelection, LoaderVersionMode};
pub use mods::{ModOrigin, ModProvider};
pub use operation::{Operation, OperationStatus, Progress};
pub use server::{InvalidServerRelativePath, ServerInstallation, ServerLaunch, ServerRelativePath};
pub use storage::StorageClass;
