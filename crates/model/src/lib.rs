//! Domain model shared by every Ephervex crate.
//!
//! This crate is the root of the dependency graph: it depends on no other
//! workspace crate, and every other crate may depend on it. It holds types
//! only - no networking, no filesystem access, no business logic.

pub mod account;
pub mod app;
pub mod cache;
pub mod hash;
pub mod ids;
pub mod instance;
pub mod java;
pub mod loader;
pub mod mods;
pub mod operation;
pub mod secret;
pub mod server;
pub mod storage;

pub use account::AccountKind;
pub use app::{APP_ID, APP_NAME, APP_URL, APP_VERSION, user_agent};
pub use cache::{
    ARTIFACT_CACHE_LIMIT_PRESETS, ArtifactCacheLimit, ArtifactCacheLimitError, ByteUnit,
    CLEANUP_TARGET_PERCENT, DEFAULT_ARTIFACT_CACHE_LIMIT_BYTES, GIB, KIB,
    MAX_ARTIFACT_CACHE_LIMIT_BYTES, MIB, TIB, format_binary_bytes,
};
pub use hash::{Hash, HashAlgorithm};
pub use ids::{InstanceId, OperationId, ServerId, SnapshotId};
pub use instance::{AttentionReason, ImportSourceKind, InstanceKind};
pub use java::{JavaOverride, JvmSettings};
pub use loader::{LoaderKind, LoaderSelection, LoaderVersionMode, default_instance_name};
pub use mods::{
    AbsentModPolicy, CurseForgeFileId, CurseForgeProjectId, DefaultModCatalog, ModOrigin,
    ModProvider, ModRelationKind,
};
pub use operation::{Operation, OperationStatus, Progress};
pub use secret::{REDACTED, SecretString};
pub use server::{
    ConnectionTarget, InvalidOperatorPermissionLevel, InvalidServerRelativePath,
    OperatorPermissionLevel, ServerInstallation, ServerLaunch, ServerRelativePath,
};
pub use storage::StorageClass;
