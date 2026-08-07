//! The error type frontends see.
//!
//! Frontends must not have to match on `ephervex-storage`'s error enums, or
//! `ephervex-download`'s, or any other provider's — that would make the layer
//! below `core` part of the frontend's compile-time surface, which is exactly
//! what the dependency direction exists to prevent.
//!
//! So every provider error is wrapped here. What crosses the boundary is a
//! stable message id plus the technical chain, and the frontend resolves the
//! id through `ephervex-i18n`. Nothing in this crate knows what
//! language the user reads.

use std::path::PathBuf;

use ephervex_storage::StorageError;

/// Anything an application service can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// Paths, configuration, the index, or the registry.
    #[error(transparent)]
    Storage(#[from] StorageError),
}

impl CoreError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    ///
    /// Delegating rather than re-deriving keeps one message id per failure
    /// across the whole stack, so the CLI and the GUI cannot drift into
    /// describing the same problem differently.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::Storage(error) => error.message_id(),
        }
    }

    /// The path this failure is about, where it names one.
    ///
    /// Frontends want to tell the user *which file* to look at, and the answer
    /// is buried at varying depths inside the provider error enums. Digging it
    /// out here rather than in the frontend is what keeps `ConfigError`,
    /// `RegistryError`, `IndexError`, and friends off a frontend's compile-time
    /// surface — otherwise "show the path" quietly drags the whole storage
    /// error hierarchy across the boundary.
    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::Storage(error) => storage_failing_path(error),
        }
    }
}

/// One function per error type rather than one deep flattened match: the arms
/// of a flattened version repeat identically across variants.
fn storage_failing_path(error: &StorageError) -> Option<PathBuf> {
    match error {
        StorageError::Config(inner) => config_failing_path(inner),
        StorageError::Registry(inner) => registry_failing_path(inner),
        StorageError::Path(inner) => path_failing_path(inner),
    }
}

fn config_failing_path(error: &ephervex_storage::ConfigError) -> Option<PathBuf> {
    use ephervex_storage::ConfigError;
    match error {
        ConfigError::Unreadable { path, .. }
        | ConfigError::Invalid { path, .. }
        | ConfigError::Unwritable { path, .. } => Some(path.clone()),
        ConfigError::Path(inner) => path_failing_path(inner),
        ConfigError::Serialization(_)
        | ConfigError::UnsupportedSchema { .. }
        | ConfigError::SecretRejected { .. } => None,
    }
}

fn registry_failing_path(error: &ephervex_storage::RegistryError) -> Option<PathBuf> {
    use ephervex_storage::{IndexError, RegistryError};
    match error {
        RegistryError::RootOccupied { path }
        | RegistryError::Io { path, .. }
        | RegistryError::Index(IndexError::Unopenable { path, .. }) => Some(path.clone()),
        RegistryError::Metadata(inner) => metadata_failing_path(inner),
        RegistryError::Path(inner) => path_failing_path(inner),
        RegistryError::Index(inner) => index_failing_path(inner),
        RegistryError::InstanceUnknown { .. } | RegistryError::ServerUnknown { .. } => None,
    }
}

fn metadata_failing_path(error: &ephervex_storage::MetadataError) -> Option<PathBuf> {
    use ephervex_storage::MetadataError;
    match error {
        MetadataError::Unwritable { path, .. }
        | MetadataError::Unreadable { path, .. }
        | MetadataError::Invalid { path, .. }
        | MetadataError::UnsupportedVersion { path, .. } => Some(path.clone()),
        MetadataError::Path(inner) => path_failing_path(inner),
    }
}

fn index_failing_path(error: &ephervex_storage::IndexError) -> Option<PathBuf> {
    use ephervex_storage::IndexError;
    match error {
        IndexError::Unopenable { path, .. } => Some(path.clone()),
        IndexError::Path(inner) => path_failing_path(inner),
        IndexError::Query(_)
        | IndexError::Migration { .. }
        | IndexError::SchemaFromFuture { .. }
        | IndexError::MalformedRow { .. } => None,
    }
}

fn path_failing_path(error: &ephervex_storage::PathError) -> Option<PathBuf> {
    use ephervex_storage::PathError;
    match error {
        PathError::DirectoryCreation { path, .. } => Some(path.clone()),
        PathError::NoHomeDirectory => None,
    }
}

impl From<ephervex_storage::RegistryError> for CoreError {
    fn from(error: ephervex_storage::RegistryError) -> Self {
        Self::Storage(StorageError::Registry(error))
    }
}

impl From<ephervex_storage::ConfigError> for CoreError {
    fn from(error: ephervex_storage::ConfigError) -> Self {
        Self::Storage(StorageError::Config(error))
    }
}

impl From<ephervex_storage::PathError> for CoreError {
    fn from(error: ephervex_storage::PathError) -> Self {
        Self::Storage(StorageError::Path(error))
    }
}

/// Shorthand for a service result.
pub type CoreResult<T> = Result<T, CoreError>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn a_wrapped_storage_error_keeps_the_message_id_storage_gave_it() {
        let inner = ephervex_storage::PathError::NoHomeDirectory;
        let expected = inner.message_id();
        let wrapped = CoreError::from(inner);
        assert_eq!(wrapped.message_id(), expected);
    }
}
