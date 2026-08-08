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

use ephervex_auth::AuthError;
use ephervex_java::JavaError;
use ephervex_minecraft::MinecraftError;
use ephervex_storage::StorageError;

/// Anything an application service can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// Paths, configuration, the index, or the registry.
    #[error(transparent)]
    Storage(#[from] StorageError),

    /// Mojang version metadata: the manifest, the release filter, or one
    /// version's document.
    #[error(transparent)]
    Minecraft(#[from] MinecraftError),

    /// Java runtime discovery, selection, or managed installation.
    #[error(transparent)]
    Java(#[from] JavaError),

    /// The Microsoft/Xbox/Minecraft token chain, or credential storage.
    #[error(transparent)]
    Auth(#[from] AuthError),
}

impl CoreError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    ///
    /// Delegating rather than re-deriving keeps one message id per failure
    /// across the whole stack, so the CLI and the GUI cannot drift into
    /// describing the same problem differently.
    #[must_use]
    pub fn message_id(&self) -> &'static str {
        match self {
            Self::Storage(error) => error.message_id(),
            Self::Minecraft(error) => error.message_id(),
            Self::Java(error) => error.message_id(),
            // Not `const`: this one chooses between message ids on the HTTP
            // status, so that a temporary outage and a permanent refusal do not
            // share a sentence.
            Self::Auth(error) => error.message_id(),
        }
    }

    /// Whether the user has to sign in again before this can succeed.
    ///
    /// Exists for the same reason as [`Self::failing_path`]: a frontend that
    /// wants to show a "Sign in" button must not have to match on `AuthError`
    /// to know it should, because doing so would put the whole authentication
    /// error hierarchy on its compile-time surface. Vanilla launch support's
    /// `LaunchError::AuthenticationRequired` is this predicate wearing a
    /// different name.
    #[must_use]
    pub const fn requires_interactive_login(&self) -> bool {
        matches!(self, Self::Auth(error) if error.requires_interactive_login())
    }

    /// Whether offering a session-only login is a sensible next step.
    ///
    /// True only when secure credential storage is missing. Callers use this to
    /// offer to be explicit rather than a silent fallback, so the decision is
    /// made once here instead of by each frontend.
    #[must_use]
    pub const fn session_only_login_would_help(&self) -> bool {
        matches!(self, Self::Auth(error) if error.session_only_would_help())
    }

    /// Whether the user canceled rather than something failing.
    #[must_use]
    pub const fn is_cancellation(&self) -> bool {
        matches!(self, Self::Auth(error) if error.is_cancellation())
    }

    /// Whether this installation cannot authenticate until its Azure
    /// application registration is approved for the Minecraft Services API.
    ///
    /// Worth a predicate of its own because it is the one authentication
    /// failure no user action inside the launcher can resolve — not signing in
    /// again, not waiting, not switching accounts. A frontend showing a
    /// "Sign in" button for it would be offering a button that cannot work.
    #[must_use]
    pub const fn needs_app_registration_approval(&self) -> bool {
        matches!(self, Self::Auth(error) if error.needs_app_registration_approval())
    }

    /// A page the user has to visit to resolve this failure, where one exists.
    ///
    /// The same idea as [`Self::failing_path`], for the failures that send a
    /// user to a web page rather than to a file. The URL is untranslated
    /// technical text that the localized message takes as an argument, so a
    /// frontend needs it without matching on `AuthError`.
    #[must_use]
    pub const fn help_url(&self) -> Option<&'static str> {
        match self {
            Self::Auth(error) => error.help_url(),
            _ => None,
        }
    }

    /// The value the user has to supply at [`Self::help_url`], where the
    /// failure names one — for the approval form, the Client ID.
    #[must_use]
    pub fn help_subject(&self) -> Option<&str> {
        match self {
            Self::Auth(error) => error.help_subject(),
            _ => None,
        }
    }

    /// The Java major version this failure is about, where it names one.
    ///
    /// Exists for the same reason as [`Self::failing_path`]: a frontend that
    /// wants to offer "download Java 21" must not have to match on `JavaError`
    /// to find the number, because doing so would put the whole Java error
    /// hierarchy on its compile-time surface.
    #[must_use]
    pub const fn required_java_major(&self) -> Option<u32> {
        match self {
            Self::Java(error) => error.required_major(),
            Self::Storage(_) | Self::Minecraft(_) | Self::Auth(_) => None,
        }
    }

    /// Whether downloading a managed runtime is a sensible next step.
    #[must_use]
    pub const fn managed_java_would_help(&self) -> bool {
        matches!(self, Self::Java(error) if error.managed_download_would_help())
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
            Self::Minecraft(error) => error.failing_path(),
            Self::Java(error) => error.failing_path(),
            // Authentication failures are never about a file. The one thing
            // that would be — the keyring entry — is not a path the user can
            // usefully be pointed at.
            Self::Auth(_) => None,
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

    #[test]
    fn a_wrapped_minecraft_error_keeps_its_own_message_id_and_path() {
        let inner = MinecraftError::unsupported("1.7.10");
        let expected = inner.message_id();
        assert_eq!(CoreError::from(inner).message_id(), expected);

        let unreadable = MinecraftError::VersionMetadataUnreadable {
            path: PathBuf::from("/cache/ephervex/metadata/sha1/ab/abc.json"),
            source: std::io::Error::other("denied"),
        };
        assert_eq!(
            CoreError::from(unreadable).failing_path(),
            Some(PathBuf::from("/cache/ephervex/metadata/sha1/ab/abc.json")),
            "a frontend must be able to name the file without matching on MinecraftError"
        );
    }

    #[test]
    fn every_message_id_core_can_hand_out_exists_in_the_bundled_resources() {
        // The frontend resolves these through `ephervex-i18n`; an id with no
        // message renders as a placeholder in the user's face.
        for id in [
            MinecraftError::unsupported("x").message_id(),
            MinecraftError::UnknownVersion { id: String::new() }.message_id(),
            MinecraftError::VersionMetadataCorrupt { id: String::new() }.message_id(),
            ephervex_storage::PathError::NoHomeDirectory.message_id(),
        ] {
            assert!(ephervex_i18n::has(id), "missing message id: {id}");
        }
    }
}
